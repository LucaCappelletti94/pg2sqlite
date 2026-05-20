//! Implementation of the [`Translator`] trait for the
//! `Query`, `SetExpr`, and `Select` types.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, Distinct, Expr, Fetch, Function, FunctionArgumentList, FunctionArguments,
    GroupByExpr, GroupByWithModifier, Ident, LimitClause, ObjectName, ObjectNamePart, OrderBy,
    OrderByExpr, OrderByKind, PipeOperator, Query, Select, SelectItem, SetExpr, SetOperator,
    SetQuantifier, Setting, TableAlias, TableFactor, TableWithJoins, Value, ValueWithSpan,
    WindowSpec, WindowType, helpers::attached_token::AttachedToken,
};

use super::helpers::{
    Forward, translate_fetch_clause, translate_limit_clause as translate_limit_clause_shared,
    translate_order_by_clause, translate_pipe_operators, translate_query_settings,
    translate_with_clause,
};
use crate::{
    impls::function_helpers::integer_literal,
    prelude::{Pg2SqliteOptions, Translator},
};

const DISTINCT_ON_DERIVED_ALIAS: &str = "__pg2sqlite_distinct_on";
const DISTINCT_ON_ROWNUM_ALIAS: &str = "__pg2sqlite_rn";

impl Translator for Query {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Query;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        let with = translate_with_clause(self.with.as_ref(), schema, options)?;
        let order_by = translate_order_by(self.order_by.as_ref(), schema, options)?;
        let limit_clause =
            translate_limit_clause_shared(self.limit_clause.as_ref(), schema, options)?;
        let fetch = translate_fetch_clause(self.fetch.as_ref(), schema, options)?;
        let settings = translate_query_settings(self.settings.as_ref(), schema, options)?;
        let pipe_operators = translate_pipe_operators(&self.pipe_operators, schema, options)?;

        if let Some(rewritten) = try_translate_distinct_on_query(
            self,
            schema,
            options,
            with.clone(),
            order_by.clone(),
            limit_clause.clone(),
            fetch.clone(),
            settings.clone(),
            pipe_operators.clone(),
        )? {
            return Ok(rewritten);
        }

        if let Some(rewritten) = try_translate_grouping_query(
            self,
            schema,
            options,
            with.clone(),
            order_by.clone(),
            limit_clause.clone(),
            fetch.clone(),
            settings.clone(),
            pipe_operators.clone(),
        )? {
            return Ok(rewritten);
        }

        Ok(build_query_envelope(
            self.body.translate(schema, options)?,
            with,
            order_by,
            limit_clause,
            fetch,
            settings,
            self.format_clause.clone(),
            pipe_operators,
        ))
    }
}

fn translate_order_by(
    order_by: Option<&OrderBy>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<OrderBy>, crate::errors::Error> {
    translate_order_by_clause(order_by, schema, options)
}

/// Build a `Query` envelope with the translated top-level clauses and the given
/// body. Strips FOR UPDATE / FOR SHARE (SQLite has no row-level locking).
#[allow(clippy::too_many_arguments)]
fn build_query_envelope(
    body: SetExpr,
    with: Option<sqlparser::ast::With>,
    order_by: Option<OrderBy>,
    limit_clause: Option<LimitClause>,
    fetch: Option<Fetch>,
    settings: Option<Vec<Setting>>,
    format_clause: Option<sqlparser::ast::FormatClause>,
    pipe_operators: Vec<PipeOperator>,
) -> Query {
    Query {
        with,
        body: Box::new(body),
        order_by,
        limit_clause,
        fetch,
        locks: vec![],
        for_clause: None,
        settings,
        format_clause,
        pipe_operators,
    }
}

fn ensure_distinct_on_projection_is_rewriteable(
    projection: &[SelectItem],
) -> Result<Vec<SelectItem>, crate::errors::Error> {
    let mut seen = std::collections::HashSet::new();
    let mut named_projection = Vec::with_capacity(projection.len());

    for item in projection {
        let (expr, alias) = match item {
            SelectItem::ExprWithAlias { expr, alias } => (expr.clone(), alias.clone()),
            SelectItem::UnnamedExpr(Expr::Identifier(ident)) => {
                (Expr::Identifier(ident.clone()), ident.clone())
            }
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => {
                let alias = parts.last().cloned().ok_or_else(|| {
                    crate::errors::Error::UnsupportedSQLiteFeature(
                        "DISTINCT ON projection contains an empty compound identifier".to_string(),
                    )
                })?;
                (Expr::CompoundIdentifier(parts.clone()), alias)
            }
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "DISTINCT ON rewrite supports only projections that can be named explicitly. \
                     Wildcards are not supported."
                        .to_string(),
                ));
            }
            SelectItem::ExprWithAliases { .. } => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "DISTINCT ON rewrite does not support multi-alias projections \
                     (Spark `expr AS (a, b)` form)"
                        .to_string(),
                ));
            }
            SelectItem::UnnamedExpr(other_expr) => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "DISTINCT ON rewrite supports only named/identifier projections. \
                     Unsupported projection item: {other_expr}",
                )));
            }
        };

        if !seen.insert(alias.value.to_lowercase()) {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "DISTINCT ON rewrite requires unique output column names".to_string(),
            ));
        }

        named_projection.push(SelectItem::ExprWithAlias { expr, alias });
    }

    Ok(named_projection)
}

fn projection_aliases(projection: &[SelectItem]) -> Result<Vec<Ident>, crate::errors::Error> {
    projection
        .iter()
        .map(|item| {
            match item {
                SelectItem::ExprWithAlias { alias, .. } => Ok(alias.clone()),
                _ => {
                    Err(crate::errors::Error::UnsupportedSQLiteFeature(
                        "DISTINCT ON rewrite expected aliased projection items".to_string(),
                    ))
                }
            }
        })
        .collect()
}

fn row_number_expr(partition_by: Vec<Expr>, order_by: Vec<OrderByExpr>) -> Expr {
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("ROW_NUMBER"))]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: Some(WindowType::WindowSpec(WindowSpec {
            window_name: None,
            partition_by,
            order_by,
            window_frame: None,
        })),
        within_group: vec![],
    })
}

fn null_literal() -> Expr {
    Expr::Value(ValueWithSpan { value: Value::Null, span: sqlparser::tokenizer::Span::empty() })
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn try_translate_distinct_on_query(
    query: &Query,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
    with: Option<sqlparser::ast::With>,
    order_by: Option<OrderBy>,
    limit_clause: Option<LimitClause>,
    fetch: Option<Fetch>,
    settings: Option<Vec<Setting>>,
    pipe_operators: Vec<PipeOperator>,
) -> Result<Option<Query>, crate::errors::Error> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Ok(None);
    };
    let Some(Distinct::On(distinct_on_exprs)) = select.distinct.as_ref() else {
        return Ok(None);
    };

    if select.top.is_some() {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
            "DISTINCT ON rewrite does not support TOP clauses".to_string(),
        ));
    }

    let mut inner_select = select.as_ref().clone();
    inner_select.distinct = None;
    let mut translated_inner = inner_select.translate(schema, options)?;

    translated_inner.projection =
        ensure_distinct_on_projection_is_rewriteable(&translated_inner.projection)?;
    let projection_aliases = projection_aliases(&translated_inner.projection)?;

    let partition_by = distinct_on_exprs
        .iter()
        .map(|expr| expr.translate(schema, options))
        .collect::<Result<Vec<_>, _>>()?;

    let window_order = match &order_by {
        Some(ob) => {
            match &ob.kind {
                OrderByKind::Expressions(exprs) => exprs.clone(),
                OrderByKind::All(_) => {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                        "DISTINCT ON rewrite does not support ORDER BY ALL".to_string(),
                    ));
                }
            }
        }
        None => Vec::new(),
    };

    translated_inner.projection.push(SelectItem::ExprWithAlias {
        expr: row_number_expr(partition_by, window_order),
        alias: Ident::new(DISTINCT_ON_ROWNUM_ALIAS),
    });

    let inner_query = Query {
        with: None,
        body: Box::new(SetExpr::Select(Box::new(translated_inner))),
        order_by: None,
        limit_clause: None,
        fetch: None,
        locks: Vec::new(),
        for_clause: None,
        settings: None,
        format_clause: None,
        pipe_operators: Vec::new(),
    };

    let derived_alias = Ident::new(DISTINCT_ON_DERIVED_ALIAS);
    let from = vec![TableWithJoins {
        relation: TableFactor::Derived {
            lateral: false,
            subquery: Box::new(inner_query),
            alias: Some(TableAlias {
                explicit: false,
                name: derived_alias.clone(),
                columns: vec![],
                at: None,
            }),
            sample: None,
        },
        joins: Vec::new(),
    }];

    let outer_projection = projection_aliases
        .iter()
        .map(|alias| {
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(vec![
                derived_alias.clone(),
                alias.clone(),
            ]))
        })
        .collect::<Vec<_>>();

    let outer_select = Select {
        select_token: AttachedToken::empty(),
        distinct: None,
        top: None,
        top_before_distinct: false,
        projection: outer_projection,
        into: None,
        from,
        lateral_views: Vec::new(),
        prewhere: None,
        selection: Some(Expr::BinaryOp {
            left: Box::new(Expr::CompoundIdentifier(vec![
                derived_alias,
                Ident::new(DISTINCT_ON_ROWNUM_ALIAS),
            ])),
            op: BinaryOperator::Eq,
            right: Box::new(integer_literal(1)),
        }),
        group_by: GroupByExpr::Expressions(Vec::new(), Vec::new()),
        cluster_by: Vec::new(),
        distribute_by: Vec::new(),
        sort_by: Vec::new(),
        having: None,
        named_window: Vec::new(),
        qualify: None,
        window_before_qualify: false,
        value_table_mode: None,
        connect_by: Vec::new(),
        flavor: select.flavor,
        exclude: None,
        optimizer_hints: Vec::new(),
        select_modifiers: None,
    };

    Ok(Some(build_query_envelope(
        SetExpr::Select(Box::new(outer_select)),
        with,
        order_by,
        limit_clause,
        fetch,
        settings,
        query.format_clause.clone(),
        pipe_operators,
    )))
}

#[derive(Clone, Copy)]
enum GroupingRewriteKind {
    GroupingSets,
    Rollup,
    Cube,
}

fn expand_rollup(elements: &[Vec<Expr>]) -> Vec<Vec<Expr>> {
    let mut sets = Vec::with_capacity(elements.len() + 1);
    for keep in (0..=elements.len()).rev() {
        let mut set = Vec::new();
        for element in elements.iter().take(keep) {
            set.extend(element.clone());
        }
        sets.push(set);
    }
    sets
}

fn expand_cube(elements: &[Vec<Expr>]) -> Result<Vec<Vec<Expr>>, crate::errors::Error> {
    if elements.len() > 8 {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
            "CUBE rewrite is limited to 8 grouping elements to avoid combinatorial explosion"
                .to_string(),
        ));
    }

    let mut sets = Vec::new();
    let n = elements.len();
    for mask in 0usize..(1usize << n) {
        let mut set = Vec::new();
        for (idx, element) in elements.iter().enumerate() {
            if (mask & (1usize << idx)) != 0 {
                set.extend(element.clone());
            }
        }
        sets.push(set);
    }
    sets.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    Ok(sets)
}

fn aggregate_function_name(func: &Function) -> Option<String> {
    func.name.0.last().and_then(|part| part.as_ident()).map(|ident| ident.value.to_lowercase())
}

/// Aggregate function names that mark an `Expr::Function` as an aggregate
/// (and not a scalar) when no `OVER` clause is present.
const AGGREGATE_NAMES: &[&str] = &[
    "sum",
    "count",
    "avg",
    "min",
    "max",
    "total",
    "group_concat",
    "string_agg",
    "json_group_array",
    "json_group_object",
    "array_agg",
    "bool_and",
    "bool_or",
    "every",
    "json_agg",
    "jsonb_agg",
    "json_object_agg",
    "jsonb_object_agg",
    "bit_and",
    "bit_or",
    "stddev",
    "stddev_pop",
    "stddev_samp",
    "variance",
    "var_pop",
    "var_samp",
    "corr",
    "covar_pop",
    "covar_samp",
    "percentile_cont",
    "percentile_disc",
    "mode",
    "regr_slope",
    "regr_intercept",
    "regr_r2",
    "regr_avgx",
    "regr_avgy",
    "regr_sxx",
    "regr_syy",
    "regr_sxy",
    "regr_count",
    "xmlagg",
    "range_agg",
    "multirange_agg",
];

fn is_aggregate_expression(expr: &Expr) -> bool {
    match expr {
        Expr::Function(func) => {
            if func.over.is_some() {
                return false;
            }
            aggregate_function_name(func)
                .is_some_and(|name| AGGREGATE_NAMES.contains(&name.as_str()))
        }
        Expr::BinaryOp { left, right, .. } => {
            is_aggregate_expression(left) || is_aggregate_expression(right)
        }
        Expr::UnaryOp { expr, .. }
        | Expr::Nested(expr)
        | Expr::Cast { expr, .. }
        | Expr::IsNull(expr)
        | Expr::IsNotNull(expr)
        | Expr::IsTrue(expr)
        | Expr::IsNotTrue(expr)
        | Expr::IsFalse(expr)
        | Expr::IsNotFalse(expr) => is_aggregate_expression(expr),
        Expr::Case { conditions, else_result, .. } => {
            conditions.iter().any(|case_when| {
                is_aggregate_expression(&case_when.condition)
                    || is_aggregate_expression(&case_when.result)
            }) || else_result.as_ref().is_some_and(|result| is_aggregate_expression(result))
        }
        _ => false,
    }
}

fn contains_expr(targets: &[Expr], expr: &Expr) -> bool {
    targets.iter().any(|target| target == expr)
}

fn rewrite_projection_for_grouping_set(
    projection: &[SelectItem],
    active_group_keys: &[Expr],
    all_group_keys: &[Expr],
    kind: GroupingRewriteKind,
) -> Result<Vec<SelectItem>, crate::errors::Error> {
    projection
        .iter()
        .map(|item| {
            let (expr, alias) = match item {
                SelectItem::UnnamedExpr(expr) => (expr, None),
                SelectItem::ExprWithAlias { expr, alias } => (expr, Some(alias)),
                SelectItem::Wildcard(_)
                | SelectItem::QualifiedWildcard(_, _)
                | SelectItem::ExprWithAliases { .. } => {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                        "{kind_name} rewrite supports explicit single-alias projection items only",
                        kind_name = match kind {
                            GroupingRewriteKind::GroupingSets => "GROUPING SETS",
                            GroupingRewriteKind::Rollup => "ROLLUP",
                            GroupingRewriteKind::Cube => "CUBE",
                        }
                    )));
                }
            };

            if contains_expr(all_group_keys, expr) {
                if contains_expr(active_group_keys, expr) {
                    return Ok(item.clone());
                }
                return Ok(match alias {
                    Some(alias) => {
                        SelectItem::ExprWithAlias { expr: null_literal(), alias: alias.clone() }
                    }
                    None => SelectItem::UnnamedExpr(null_literal()),
                });
            }

            if is_aggregate_expression(expr)
                || matches!(expr, Expr::Value(_) | Expr::TypedString(_))
            {
                return Ok(item.clone());
            }

            Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "GROUPING SETS/ROLLUP/CUBE rewrite supports only grouping-key columns, \
                 aggregate expressions, and literals in SELECT projections"
                    .to_string(),
            ))
        })
        .collect()
}

fn union_all(set_exprs: Vec<SetExpr>) -> SetExpr {
    let mut iter = set_exprs.into_iter();
    let first = iter.next().expect("at least one set expression");
    iter.fold(first, |left, right| {
        SetExpr::SetOperation {
            op: SetOperator::Union,
            set_quantifier: SetQuantifier::All,
            left: Box::new(left),
            right: Box::new(right),
        }
    })
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn try_translate_grouping_query(
    query: &Query,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
    with: Option<sqlparser::ast::With>,
    order_by: Option<OrderBy>,
    limit_clause: Option<LimitClause>,
    fetch: Option<Fetch>,
    settings: Option<Vec<Setting>>,
    pipe_operators: Vec<PipeOperator>,
) -> Result<Option<Query>, crate::errors::Error> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Ok(None);
    };

    let GroupByExpr::Expressions(group_exprs, modifiers) = &select.group_by else {
        if matches!(&select.group_by, GroupByExpr::All(mods) if !mods.is_empty()) {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "GROUP BY ALL with modifiers is not supported in SQLite translation".to_string(),
            ));
        }
        return Ok(None);
    };

    if !modifiers.is_empty() {
        if modifiers.iter().any(|m| {
            matches!(
                m,
                GroupByWithModifier::Rollup
                    | GroupByWithModifier::Cube
                    | GroupByWithModifier::GroupingSets(_)
            )
        }) {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "GROUP BY ... WITH ROLLUP/CUBE/GROUPING SETS modifiers are not supported"
                    .to_string(),
            ));
        }
        return Ok(None);
    }

    let mut prefix_group_keys = Vec::new();
    let mut grouping_operator: Option<(GroupingRewriteKind, Vec<Vec<Expr>>)> = None;
    for expr in group_exprs {
        let grouped_sets = match expr {
            Expr::GroupingSets(sets) => Some((GroupingRewriteKind::GroupingSets, sets.clone())),
            Expr::Rollup(elements) => Some((GroupingRewriteKind::Rollup, expand_rollup(elements))),
            Expr::Cube(elements) => Some((GroupingRewriteKind::Cube, expand_cube(elements)?)),
            _ => None,
        };

        if let Some(grouped_sets) = grouped_sets {
            if grouping_operator.is_some() {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "GROUPING SETS/ROLLUP/CUBE rewrite supports at most one grouping operator per GROUP BY"
                        .to_string(),
                ));
            }
            grouping_operator = Some(grouped_sets);
        } else {
            prefix_group_keys.push(expr.clone());
        }
    }

    let Some((kind, raw_sets)) = grouping_operator else {
        return Ok(None);
    };

    let expanded_sets = raw_sets
        .into_iter()
        .map(|mut set| {
            let mut full = prefix_group_keys.clone();
            full.append(&mut set);
            full
        })
        .collect::<Vec<_>>();

    if select.distinct.is_some() {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
            "GROUPING SETS/ROLLUP/CUBE rewrite does not support DISTINCT in the same SELECT"
                .to_string(),
        ));
    }
    if select.having.is_some() {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
            "GROUPING SETS/ROLLUP/CUBE rewrite does not yet support HAVING clauses".to_string(),
        ));
    }
    if select.top.is_some() {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
            "GROUPING SETS/ROLLUP/CUBE rewrite does not support TOP clauses".to_string(),
        ));
    }

    let mut base_select = select.as_ref().clone();
    base_select.group_by = GroupByExpr::Expressions(Vec::new(), Vec::new());
    let translated_base = base_select.translate(schema, options)?;

    let translated_sets = expanded_sets
        .iter()
        .map(|set| {
            set.iter()
                .map(|expr| expr.translate(schema, options))
                .collect::<Result<Vec<_>, crate::errors::Error>>()
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut all_group_keys = Vec::new();
    for group_set in &translated_sets {
        for expr in group_set {
            if !contains_expr(&all_group_keys, expr) {
                all_group_keys.push(expr.clone());
            }
        }
    }

    let mut branches = Vec::with_capacity(translated_sets.len());
    for group_set in translated_sets {
        let projection = rewrite_projection_for_grouping_set(
            &translated_base.projection,
            &group_set,
            &all_group_keys,
            kind,
        )?;

        let mut branch_select = translated_base.clone();
        branch_select.projection = projection;
        branch_select.group_by = GroupByExpr::Expressions(group_set, Vec::new());
        branches.push(SetExpr::Select(Box::new(branch_select)));
    }

    Ok(Some(build_query_envelope(
        union_all(branches),
        with,
        order_by,
        limit_clause,
        fetch,
        settings,
        query.format_clause.clone(),
        pipe_operators,
    )))
}

impl Translator for SetExpr {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = SetExpr;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        crate::impls::shared_helpers::translate_set_expr_shared::<Forward>(self, schema, options)
    }
}

impl Translator for Select {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Select;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        crate::impls::shared_helpers::translate_select_shared::<Forward>(self, schema, options)
    }
}

/// Test-only wrappers for internal helpers.
#[cfg(test)]
fn translate_group_by(
    group_by: &sqlparser::ast::GroupByExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::GroupByExpr, crate::errors::Error> {
    crate::impls::shared_helpers::translate_group_by_expr::<Forward>(group_by, schema, options)
}

#[cfg(test)]
fn translate_limit_clause(
    limit_clause: Option<&LimitClause>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<LimitClause>, crate::errors::Error> {
    translate_limit_clause_shared(limit_clause, schema, options)
}

#[cfg(test)]
fn translate_fetch(
    fetch: Option<&Fetch>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Fetch>, crate::errors::Error> {
    translate_fetch_clause(fetch, schema, options)
}

#[cfg(test)]
fn translate_distinct(
    distinct: Option<&Distinct>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Distinct>, crate::errors::Error> {
    crate::impls::shared_helpers::translate_distinct_shared::<Forward>(distinct, schema, options)
}

#[cfg(test)]
fn translate_named_window(
    named_windows: &[sqlparser::ast::NamedWindowDefinition],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<sqlparser::ast::NamedWindowDefinition>, crate::errors::Error> {
    crate::impls::shared_helpers::translate_named_windows::<Forward>(named_windows, schema, options)
}

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{
            Distinct, GroupByExpr, NamedWindowDefinition, NamedWindowExpr, Query, SelectItem,
            SetExpr, Statement,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        GroupingRewriteKind, ensure_distinct_on_projection_is_rewriteable, expand_cube,
        is_aggregate_expression, projection_aliases, rewrite_projection_for_grouping_set,
        translate_distinct, translate_fetch, translate_limit_clause, translate_named_window,
        translate_order_by, try_translate_distinct_on_query, try_translate_grouping_query,
    };
    use crate::prelude::{Pg2SqliteOptions, Translator};

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap()
    }

    fn parse_query(sql: &str) -> Query {
        let stmt = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap().remove(0);
        match stmt {
            Statement::Query(query) => *query,
            other => panic!("expected query, got: {other:?}"),
        }
    }

    fn parse_expr(expr: &str) -> sqlparser::ast::Expr {
        Parser::new(&PostgreSqlDialect {}).try_with_sql(expr).unwrap().parse_expr().unwrap()
    }

    #[test]
    fn distinct_on_projection_rewrite_validates_inputs() {
        let valid = vec![SelectItem::ExprWithAlias {
            expr: parse_expr("user_id"),
            alias: sqlparser::ast::Ident::new("user_id"),
        }];
        assert!(ensure_distinct_on_projection_is_rewriteable(&valid).is_ok());

        let wildcard =
            vec![SelectItem::Wildcard(sqlparser::ast::WildcardAdditionalOptions::default())];
        assert!(ensure_distinct_on_projection_is_rewriteable(&wildcard).is_err());

        let duplicated = vec![
            SelectItem::ExprWithAlias {
                expr: parse_expr("user_id"),
                alias: sqlparser::ast::Ident::new("x"),
            },
            SelectItem::ExprWithAlias {
                expr: parse_expr("tenant_id"),
                alias: sqlparser::ast::Ident::new("x"),
            },
        ];
        assert!(ensure_distinct_on_projection_is_rewriteable(&duplicated).is_err());
    }

    #[test]
    fn query_translation_rejects_distinct_on_order_by_all_and_grouping_edge_cases() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        let mut distinct_on_order_all =
            parse_query("SELECT DISTINCT ON (user_id) user_id, ts FROM events ORDER BY user_id");
        distinct_on_order_all.order_by = Some(sqlparser::ast::OrderBy {
            kind: sqlparser::ast::OrderByKind::All(sqlparser::ast::OrderByOptions {
                asc: None,
                nulls_first: None,
            }),
            interpolate: None,
        });
        let err = distinct_on_order_all.translate(&schema, &options).unwrap_err();
        assert!(err.to_string().contains("ORDER BY ALL"), "unexpected error: {err}");

        let multiple_grouping_ops =
            parse_query("SELECT a, SUM(v) FROM t GROUP BY ROLLUP(a), CUBE(a)");
        let err = multiple_grouping_ops.translate(&schema, &options).unwrap_err();
        assert!(
            err.to_string().contains("at most one grouping operator"),
            "unexpected error: {err}"
        );

        let grouping_with_distinct =
            parse_query("SELECT DISTINCT a, SUM(v) FROM t GROUP BY GROUPING SETS ((a), ())");
        let err = grouping_with_distinct.translate(&schema, &options).unwrap_err();
        assert!(err.to_string().contains("does not support DISTINCT"), "unexpected error: {err}");

        let grouping_with_having =
            parse_query("SELECT a, SUM(v) FROM t GROUP BY ROLLUP(a) HAVING SUM(v) > 0");
        let err = grouping_with_having.translate(&schema, &options).unwrap_err();
        assert!(err.to_string().contains("HAVING"), "unexpected error: {err}");
    }

    #[test]
    fn grouping_helpers_cover_cube_limit_and_projection_rewrite_paths() {
        let too_many = vec![vec![parse_expr("a")]; 9];
        let err = expand_cube(&too_many).unwrap_err();
        assert!(err.to_string().contains("limited to 8"), "unexpected error: {err}");

        let projection = vec![
            SelectItem::ExprWithAlias {
                expr: parse_expr("region"),
                alias: sqlparser::ast::Ident::new("region"),
            },
            SelectItem::ExprWithAlias {
                expr: parse_expr("product"),
                alias: sqlparser::ast::Ident::new("product"),
            },
            SelectItem::ExprWithAlias {
                expr: parse_expr("SUM(amount)"),
                alias: sqlparser::ast::Ident::new("total"),
            },
        ];
        let active = vec![parse_expr("region")];
        let all = vec![parse_expr("region"), parse_expr("product")];
        let rewritten = rewrite_projection_for_grouping_set(
            &projection,
            &active,
            &all,
            GroupingRewriteKind::Rollup,
        )
        .unwrap();
        assert_eq!(rewritten.len(), 3);
        assert!(
            rewritten[1].to_string().to_uppercase().contains("NULL"),
            "expected inactive grouping key to be rewritten to NULL"
        );
    }

    #[test]
    fn low_level_translation_helpers_cover_remaining_variants() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        let order_by_all = Some(sqlparser::ast::OrderBy {
            kind: sqlparser::ast::OrderByKind::All(sqlparser::ast::OrderByOptions {
                asc: Some(true),
                nulls_first: Some(false),
            }),
            interpolate: None,
        });
        let _ = translate_order_by(order_by_all.as_ref(), &schema, &options).unwrap();

        let offset_comma_limit = Some(sqlparser::ast::LimitClause::OffsetCommaLimit {
            offset: parse_expr("5"),
            limit: parse_expr("10"),
        });
        let _ = translate_limit_clause(offset_comma_limit.as_ref(), &schema, &options).unwrap();

        let distinct_on = Some(Distinct::On(vec![parse_expr("a")]));
        let err = translate_distinct(distinct_on.as_ref(), &schema, &options).unwrap_err();
        assert!(err.to_string().contains("DISTINCT ON"), "unexpected error: {err}");

        let group_by_all = GroupByExpr::All(vec![]);
        let translated = super::translate_group_by(&group_by_all, &schema, &options).unwrap();
        assert!(matches!(translated, GroupByExpr::All(_)));
    }

    #[test]
    fn additional_query_helpers_cover_error_paths_for_distinct_and_grouping() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        let empty_compound =
            vec![SelectItem::UnnamedExpr(sqlparser::ast::Expr::CompoundIdentifier(Vec::new()))];
        assert!(ensure_distinct_on_projection_is_rewriteable(&empty_compound).is_err());

        let unsupported_item = vec![SelectItem::UnnamedExpr(parse_expr("a + 1"))];
        assert!(ensure_distinct_on_projection_is_rewriteable(&unsupported_item).is_err());
        assert!(projection_aliases(&unsupported_item).is_err());

        let wildcard_projection =
            vec![SelectItem::Wildcard(sqlparser::ast::WildcardAdditionalOptions::default())];
        assert!(
            rewrite_projection_for_grouping_set(
                &wildcard_projection,
                &[],
                &[],
                GroupingRewriteKind::Cube
            )
            .is_err()
        );

        let mut distinct_query = parse_query("SELECT DISTINCT ON (id) id FROM users ORDER BY id");
        if let SetExpr::Select(select) = distinct_query.body.as_mut() {
            select.top = Some(sqlparser::ast::Top {
                with_ties: false,
                percent: false,
                quantity: Some(sqlparser::ast::TopQuantity::Constant(1)),
            });
        }
        let err = try_translate_distinct_on_query(
            &distinct_query,
            &schema,
            &options,
            distinct_query.with.clone(),
            distinct_query.order_by.clone(),
            distinct_query.limit_clause.clone(),
            distinct_query.fetch.clone(),
            distinct_query.settings.clone(),
            distinct_query.pipe_operators.clone(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("TOP clauses"));

        let mut group_all_mod_query = parse_query("SELECT id FROM users");
        if let SetExpr::Select(select) = group_all_mod_query.body.as_mut() {
            select.group_by = GroupByExpr::All(vec![sqlparser::ast::GroupByWithModifier::Rollup]);
        }
        let err = try_translate_grouping_query(
            &group_all_mod_query,
            &schema,
            &options,
            None,
            None,
            None,
            None,
            None,
            Vec::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("GROUP BY ALL with modifiers"));

        let mut group_mod_query = parse_query("SELECT id FROM users GROUP BY id");
        if let SetExpr::Select(select) = group_mod_query.body.as_mut() {
            select.group_by = GroupByExpr::Expressions(
                vec![parse_expr("id")],
                vec![sqlparser::ast::GroupByWithModifier::Rollup],
            );
        }
        let err = try_translate_grouping_query(
            &group_mod_query,
            &schema,
            &options,
            None,
            None,
            None,
            None,
            None,
            Vec::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("WITH ROLLUP/CUBE/GROUPING SETS"));

        let mut top_rollup = parse_query("SELECT id, SUM(v) FROM t GROUP BY ROLLUP(id)");
        if let SetExpr::Select(select) = top_rollup.body.as_mut() {
            select.top = Some(sqlparser::ast::Top {
                with_ties: false,
                percent: false,
                quantity: Some(sqlparser::ast::TopQuantity::Constant(1)),
            });
        }
        let err = try_translate_grouping_query(
            &top_rollup,
            &schema,
            &options,
            None,
            None,
            None,
            None,
            None,
            Vec::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("TOP clauses"));
    }

    #[test]
    fn grouping_rewrite_errors_include_grouping_sets_and_rollup_kind_names() {
        let wildcard_projection =
            vec![SelectItem::Wildcard(sqlparser::ast::WildcardAdditionalOptions::default())];
        assert!(
            rewrite_projection_for_grouping_set(
                &wildcard_projection,
                &[],
                &[],
                GroupingRewriteKind::GroupingSets
            )
            .is_err()
        );
        assert!(
            rewrite_projection_for_grouping_set(
                &wildcard_projection,
                &[],
                &[],
                GroupingRewriteKind::Rollup
            )
            .is_err()
        );
    }

    #[test]
    fn additional_query_helpers_cover_modifier_and_named_window_paths() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        assert!(!is_aggregate_expression(&parse_expr("sum(v) OVER (PARTITION BY id)")));
        assert!(is_aggregate_expression(&parse_expr("bool_and(active)")));
        assert!(is_aggregate_expression(&parse_expr("bool_or(active)")));
        assert!(is_aggregate_expression(&parse_expr("every(active)")));
        assert!(is_aggregate_expression(&parse_expr("CASE WHEN 1=1 THEN avg(v) ELSE 0 END")));
        assert!(is_aggregate_expression(&parse_expr("NOT max(v)")));
        assert!(is_aggregate_expression(&parse_expr("(max(v))")));
        assert!(is_aggregate_expression(&parse_expr("max(v)::INT")));
        assert!(is_aggregate_expression(&parse_expr("max(v) IS NULL")));
        assert!(is_aggregate_expression(&parse_expr("max(v) IS NOT NULL")));
        assert!(is_aggregate_expression(&parse_expr("max(v) IS TRUE")));
        assert!(is_aggregate_expression(&parse_expr("max(v) IS NOT TRUE")));
        assert!(is_aggregate_expression(&parse_expr("max(v) IS FALSE")));
        assert!(is_aggregate_expression(&parse_expr("max(v) IS NOT FALSE")));

        let limit_offset = Some(sqlparser::ast::LimitClause::LimitOffset {
            limit: Some(parse_expr("10")),
            offset: Some(sqlparser::ast::Offset {
                value: parse_expr("1"),
                rows: sqlparser::ast::OffsetRows::None,
            }),
            limit_by: vec![parse_expr("2")],
        });
        let _ = translate_limit_clause(limit_offset.as_ref(), &schema, &options).unwrap();

        let fetch = Some(sqlparser::ast::Fetch {
            with_ties: false,
            percent: false,
            quantity: Some(parse_expr("3")),
        });
        let _ = translate_fetch(fetch.as_ref(), &schema, &options).unwrap();

        let _ = translate_distinct(Some(&Distinct::All), &schema, &options).unwrap();

        let named_window_query =
            parse_query("SELECT sum(v) OVER w FROM t WINDOW w AS (PARTITION BY id)");
        let SetExpr::Select(select) = named_window_query.body.as_ref() else {
            panic!("expected select");
        };
        let translated_named =
            translate_named_window(&select.named_window, &schema, &options).unwrap();
        assert_eq!(translated_named.len(), 1);
    }

    #[test]
    fn additional_query_helpers_cover_remaining_non_error_paths() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        let compound_projection =
            vec![SelectItem::UnnamedExpr(sqlparser::ast::Expr::CompoundIdentifier(vec![
                sqlparser::ast::Ident::new("t"),
                sqlparser::ast::Ident::new("id"),
            ]))];
        let rewritten = ensure_distinct_on_projection_is_rewriteable(&compound_projection).unwrap();
        assert!(matches!(rewritten[0], SelectItem::ExprWithAlias { .. }));

        let distinct_no_order = parse_query("SELECT DISTINCT ON (id) id FROM users");
        let rewritten = try_translate_distinct_on_query(
            &distinct_no_order,
            &schema,
            &options,
            distinct_no_order.with.clone(),
            None,
            None,
            None,
            distinct_no_order.settings.clone(),
            distinct_no_order.pipe_operators.clone(),
        )
        .unwrap();
        assert!(rewritten.is_some());

        let mut group_by_all = parse_query("SELECT id FROM users");
        if let SetExpr::Select(select) = group_by_all.body.as_mut() {
            select.group_by = GroupByExpr::All(Vec::new());
        }
        let result = try_translate_grouping_query(
            &group_by_all,
            &schema,
            &options,
            None,
            None,
            None,
            None,
            None,
            Vec::new(),
        )
        .unwrap();
        assert!(result.is_none());

        let mut group_with_totals = parse_query("SELECT id FROM users GROUP BY id");
        if let SetExpr::Select(select) = group_with_totals.body.as_mut() {
            select.group_by = GroupByExpr::Expressions(
                vec![parse_expr("id")],
                vec![sqlparser::ast::GroupByWithModifier::Totals],
            );
        }
        let result = try_translate_grouping_query(
            &group_with_totals,
            &schema,
            &options,
            None,
            None,
            None,
            None,
            None,
            Vec::new(),
        )
        .unwrap();
        assert!(result.is_none());

        let set_query = SetExpr::Query(Box::new(parse_query("SELECT 1")));
        assert!(matches!(set_query.translate(&schema, &options).unwrap(), SetExpr::Query(_)));

        let set_table = SetExpr::Table(Box::new(sqlparser::ast::Table {
            table_name: Some("users".to_string()),
            schema_name: None,
        }));
        assert!(set_table.translate(&schema, &options).is_err());

        let _ = translate_distinct(Some(&Distinct::Distinct), &schema, &options).unwrap();

        let named_ref_query =
            parse_query("SELECT sum(v) OVER w2 FROM t WINDOW w1 AS (PARTITION BY id), w2 AS (w1)");
        let SetExpr::Select(select) = named_ref_query.body.as_ref() else {
            panic!("expected select");
        };
        let translated = translate_named_window(&select.named_window, &schema, &options).unwrap();
        assert_eq!(translated.len(), 2);
    }

    #[test]
    fn named_window_translation_supports_named_window_expr_variant_directly() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let windows = vec![NamedWindowDefinition(
            sqlparser::ast::Ident::new("w2"),
            NamedWindowExpr::NamedWindow(sqlparser::ast::Ident::new("w1")),
        )];
        let translated = translate_named_window(&windows, &schema, &options).unwrap();
        assert_eq!(translated.len(), 1);
        assert!(matches!(translated[0].1, NamedWindowExpr::NamedWindow(_)));
    }

    #[test]
    fn query_translation_translates_select_side_and_query_level_expression_paths() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        let mut query = parse_query("SELECT id FROM users");
        let SetExpr::Select(select) = query.body.as_mut() else {
            panic!("expected select");
        };

        select.prewhere = Some(parse_expr("now()"));
        select.cluster_by = vec![parse_expr("now()")];
        select.distribute_by = vec![parse_expr("now()")];
        select.sort_by = vec![sqlparser::ast::OrderByExpr {
            expr: parse_expr("now()"),
            options: sqlparser::ast::OrderByOptions { asc: Some(true), nulls_first: Some(false) },
            with_fill: None,
        }];
        select.connect_by = vec![
            sqlparser::ast::ConnectByKind::ConnectBy {
                connect_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                nocycle: false,
                relationships: vec![parse_expr("now()")],
            },
            sqlparser::ast::ConnectByKind::StartWith {
                start_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                condition: Box::new(parse_expr("now()")),
            },
        ];

        query.settings = Some(vec![sqlparser::ast::Setting {
            key: sqlparser::ast::Ident::new("x"),
            value: parse_expr("now()"),
        }]);
        query.pipe_operators = vec![
            sqlparser::ast::PipeOperator::Where { expr: parse_expr("now()") },
            sqlparser::ast::PipeOperator::Union {
                set_quantifier: sqlparser::ast::SetQuantifier::All,
                queries: vec![parse_query("SELECT now() AS x")],
            },
        ];

        let translated = query.translate(&schema, &options).unwrap();
        let SetExpr::Select(select) = translated.body.as_ref() else {
            panic!("expected translated select");
        };

        assert!(
            select
                .prewhere
                .as_ref()
                .is_some_and(|expr| expr.to_string().contains("datetime('now')"))
        );
        assert!(select.cluster_by[0].to_string().contains("datetime('now')"));
        assert!(select.distribute_by[0].to_string().contains("datetime('now')"));
        assert!(select.sort_by[0].expr.to_string().contains("datetime('now')"));

        match &select.connect_by[0] {
            sqlparser::ast::ConnectByKind::ConnectBy { relationships, .. } => {
                assert!(relationships[0].to_string().contains("datetime('now')"));
            }
            sqlparser::ast::ConnectByKind::StartWith { .. } => {
                panic!("unexpected connect by variant: {:?}", select.connect_by[0]);
            }
        }
        match &select.connect_by[1] {
            sqlparser::ast::ConnectByKind::StartWith { condition, .. } => {
                assert!(condition.to_string().contains("datetime('now')"));
            }
            sqlparser::ast::ConnectByKind::ConnectBy { .. } => {
                panic!("unexpected connect by variant: {:?}", select.connect_by[1]);
            }
        }

        assert!(
            translated
                .settings
                .as_ref()
                .is_some_and(|settings| settings[0].value.to_string().contains("datetime('now')"))
        );

        match &translated.pipe_operators[0] {
            sqlparser::ast::PipeOperator::Where { expr } => {
                assert!(expr.to_string().contains("datetime('now')"));
            }
            other => panic!("unexpected first pipe operator variant: {other:?}"),
        }
        match &translated.pipe_operators[1] {
            sqlparser::ast::PipeOperator::Union { queries, .. } => {
                assert!(queries[0].to_string().contains("datetime('now')"));
            }
            other => panic!("unexpected second pipe operator variant: {other:?}"),
        }
    }
}
