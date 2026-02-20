//! Implementation of the [`Translator`] trait for the
//! `Query`, `SetExpr`, and `Select` types.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, Distinct, Expr, Fetch, Function, FunctionArgumentList, FunctionArguments,
    GroupByExpr, GroupByWithModifier, Ident, LimitClause, NamedWindowDefinition, NamedWindowExpr,
    ObjectName, ObjectNamePart, Offset, OrderBy, OrderByExpr, OrderByKind, Query, Select,
    SelectItem, SetExpr, SetOperator, SetQuantifier, TableAlias, TableFactor, TableWithJoins,
    Value, ValueWithSpan, Values, WindowSpec, WindowType, helpers::attached_token::AttachedToken,
};

use super::helpers::{translate_select_item, translate_table_with_joins};
use crate::prelude::{Pg2SqliteOptions, Translator};

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
        let with = translate_with(self.with.as_ref(), schema, options)?;
        let order_by = translate_order_by(self.order_by.as_ref(), schema, options)?;
        let limit_clause = translate_limit_clause(self.limit_clause.as_ref(), schema, options)?;
        let fetch = translate_fetch(self.fetch.as_ref(), schema, options)?;

        if let Some(rewritten) = try_translate_distinct_on_query(
            self,
            schema,
            options,
            with.clone(),
            order_by.clone(),
            limit_clause.clone(),
            fetch.clone(),
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
        )? {
            return Ok(rewritten);
        }

        Ok(Query {
            with,
            body: Box::new(self.body.translate(schema, options)?),
            order_by,
            limit_clause,
            fetch,
            locks: self.locks.clone(),
            for_clause: self.for_clause.clone(),
            settings: self.settings.clone(),
            format_clause: self.format_clause.clone(),
            pipe_operators: self.pipe_operators.clone(),
        })
    }
}

fn translate_order_by(
    order_by: Option<&OrderBy>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<OrderBy>, crate::errors::Error> {
    order_by
        .map(|ob| -> Result<OrderBy, crate::errors::Error> {
            let kind = match &ob.kind {
                OrderByKind::Expressions(exprs) => {
                    let translated_exprs = exprs
                        .iter()
                        .map(|expr| expr.translate(schema, options))
                        .collect::<Result<Vec<_>, _>>()?;
                    OrderByKind::Expressions(translated_exprs)
                }
                OrderByKind::All(all) => OrderByKind::All(*all),
            };
            Ok(OrderBy { kind, interpolate: ob.interpolate.clone() })
        })
        .transpose()
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

fn integer_literal(value: i64) -> Expr {
    Expr::Value(ValueWithSpan {
        value: Value::Number(value.to_string(), false),
        span: sqlparser::tokenizer::Span::empty(),
    })
}

fn null_literal() -> Expr {
    Expr::Value(ValueWithSpan { value: Value::Null, span: sqlparser::tokenizer::Span::empty() })
}

#[allow(clippy::too_many_lines)]
fn try_translate_distinct_on_query(
    query: &Query,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
    with: Option<sqlparser::ast::With>,
    order_by: Option<OrderBy>,
    limit_clause: Option<LimitClause>,
    fetch: Option<Fetch>,
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
        optimizer_hint: None,
        select_modifiers: None,
    };

    Ok(Some(Query {
        with,
        body: Box::new(SetExpr::Select(Box::new(outer_select))),
        order_by,
        limit_clause,
        fetch,
        locks: query.locks.clone(),
        for_clause: query.for_clause.clone(),
        settings: query.settings.clone(),
        format_clause: query.format_clause.clone(),
        pipe_operators: query.pipe_operators.clone(),
    }))
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

fn is_aggregate_expression(expr: &Expr) -> bool {
    match expr {
        Expr::Function(func) => {
            if func.over.is_some() {
                return false;
            }
            matches!(
                aggregate_function_name(func).as_deref(),
                Some(
                    "sum"
                        | "count"
                        | "avg"
                        | "min"
                        | "max"
                        | "total"
                        | "group_concat"
                        | "string_agg"
                        | "json_group_array"
                        | "json_group_object"
                        | "array_agg"
                        | "bool_and"
                        | "bool_or"
                        | "every"
                )
            )
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
                SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                        "{kind_name} rewrite supports explicit projection items only",
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

#[allow(clippy::too_many_lines)]
fn try_translate_grouping_query(
    query: &Query,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
    with: Option<sqlparser::ast::With>,
    order_by: Option<OrderBy>,
    limit_clause: Option<LimitClause>,
    fetch: Option<Fetch>,
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
    let mut grouping_operator: Option<&Expr> = None;
    for expr in group_exprs {
        if matches!(expr, Expr::GroupingSets(_) | Expr::Rollup(_) | Expr::Cube(_)) {
            if grouping_operator.is_some() {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "GROUPING SETS/ROLLUP/CUBE rewrite supports at most one grouping operator per GROUP BY"
                        .to_string(),
                ));
            }
            grouping_operator = Some(expr);
        } else {
            prefix_group_keys.push(expr.clone());
        }
    }

    let Some(grouping_operator) = grouping_operator else {
        return Ok(None);
    };

    let (kind, raw_sets) = match grouping_operator {
        Expr::GroupingSets(sets) => (GroupingRewriteKind::GroupingSets, sets.clone()),
        Expr::Rollup(elements) => (GroupingRewriteKind::Rollup, expand_rollup(elements)),
        Expr::Cube(elements) => (GroupingRewriteKind::Cube, expand_cube(elements)?),
        _ => return Ok(None),
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

    Ok(Some(Query {
        with,
        body: Box::new(union_all(branches)),
        order_by,
        limit_clause,
        fetch,
        locks: query.locks.clone(),
        for_clause: query.for_clause.clone(),
        settings: query.settings.clone(),
        format_clause: query.format_clause.clone(),
        pipe_operators: query.pipe_operators.clone(),
    }))
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
        Ok(match self {
            SetExpr::Select(select) => {
                SetExpr::Select(Box::new(select.translate(schema, options)?))
            }
            SetExpr::Query(query) => SetExpr::Query(Box::new(query.translate(schema, options)?)),
            SetExpr::SetOperation { op, set_quantifier, left, right } => {
                SetExpr::SetOperation {
                    op: *op,
                    set_quantifier: *set_quantifier,
                    left: Box::new(left.translate(schema, options)?),
                    right: Box::new(right.translate(schema, options)?),
                }
            }
            SetExpr::Values(values) => SetExpr::Values(translate_values(values, schema, options)?),
            SetExpr::Insert(_)
            | SetExpr::Table(_)
            | SetExpr::Update(_)
            | SetExpr::Delete(_)
            | SetExpr::Merge(_) => self.clone(),
        })
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
        // Translate the WHERE clause (selection) which may contain @@ expressions
        let selection =
            self.selection.as_ref().map(|expr| expr.translate(schema, options)).transpose()?;

        // Translate subqueries in FROM clause
        let from = self
            .from
            .iter()
            .map(|table_with_joins| translate_table_with_joins(table_with_joins, schema, options))
            .collect::<Result<Vec<_>, _>>()?;

        // Translate expressions in projections (SELECT clause)
        let projection = self
            .projection
            .iter()
            .map(|item| translate_select_item(item, schema, options))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Select {
            select_token: self.select_token.clone(),
            distinct: translate_distinct(self.distinct.as_ref(), schema, options)?,
            top: self.top.clone(),
            top_before_distinct: self.top_before_distinct,
            projection,
            into: self.into.clone(),
            from,
            lateral_views: self.lateral_views.clone(),
            prewhere: self.prewhere.clone(),
            selection,
            group_by: translate_group_by(&self.group_by, schema, options)?,
            cluster_by: self.cluster_by.clone(),
            distribute_by: self.distribute_by.clone(),
            sort_by: self.sort_by.clone(),
            having: self.having.as_ref().map(|expr| expr.translate(schema, options)).transpose()?,
            named_window: translate_named_window(&self.named_window, schema, options)?,
            qualify: self.qualify.as_ref().map(|e| e.translate(schema, options)).transpose()?,
            window_before_qualify: self.window_before_qualify,
            value_table_mode: self.value_table_mode,
            connect_by: self.connect_by.clone(),
            flavor: self.flavor,
            exclude: self.exclude.clone(),
            optimizer_hint: self.optimizer_hint.clone(),
            select_modifiers: self.select_modifiers.clone(),
        })
    }
}

fn translate_with(
    with: Option<&sqlparser::ast::With>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<sqlparser::ast::With>, crate::errors::Error> {
    with.map(|w| {
        let cte_tables = w
            .cte_tables
            .iter()
            .map(|cte| {
                Ok(sqlparser::ast::Cte {
                    alias: cte.alias.clone(),
                    query: Box::new(cte.query.translate(schema, options)?),
                    from: cte.from.clone(),
                    materialized: cte.materialized,
                    closing_paren_token: cte.closing_paren_token.clone(),
                })
            })
            .collect::<Result<Vec<_>, crate::errors::Error>>()?;
        Ok(sqlparser::ast::With {
            with_token: w.with_token.clone(),
            recursive: w.recursive,
            cte_tables,
        })
    })
    .transpose()
}

fn translate_group_by(
    group_by: &sqlparser::ast::GroupByExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::GroupByExpr, crate::errors::Error> {
    Ok(match group_by {
        sqlparser::ast::GroupByExpr::Expressions(exprs, modifiers) => {
            let translated = exprs
                .iter()
                .map(|e| e.translate(schema, options))
                .collect::<Result<Vec<_>, _>>()?;
            sqlparser::ast::GroupByExpr::Expressions(translated, modifiers.clone())
        }
        sqlparser::ast::GroupByExpr::All(all) => sqlparser::ast::GroupByExpr::All(all.clone()),
    })
}

fn translate_limit_clause(
    limit_clause: Option<&LimitClause>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<LimitClause>, crate::errors::Error> {
    limit_clause
        .map(|lc| {
            Ok(match lc {
                LimitClause::LimitOffset { limit, offset, limit_by } => {
                    LimitClause::LimitOffset {
                        limit: limit.as_ref().map(|e| e.translate(schema, options)).transpose()?,
                        offset: offset
                            .as_ref()
                            .map(|o| {
                                Ok::<_, crate::errors::Error>(Offset {
                                    value: o.value.translate(schema, options)?,
                                    rows: o.rows,
                                })
                            })
                            .transpose()?,
                        limit_by: limit_by
                            .iter()
                            .map(|e| e.translate(schema, options))
                            .collect::<Result<Vec<_>, _>>()?,
                    }
                }
                LimitClause::OffsetCommaLimit { offset, limit } => {
                    LimitClause::OffsetCommaLimit {
                        offset: offset.translate(schema, options)?,
                        limit: limit.translate(schema, options)?,
                    }
                }
            })
        })
        .transpose()
}

fn translate_fetch(
    fetch: Option<&Fetch>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Fetch>, crate::errors::Error> {
    fetch
        .map(|f| {
            Ok(Fetch {
                with_ties: f.with_ties,
                percent: f.percent,
                quantity: f.quantity.as_ref().map(|e| e.translate(schema, options)).transpose()?,
            })
        })
        .transpose()
}

fn translate_distinct(
    distinct: Option<&Distinct>,
    _schema: &ParserDB,
    _options: &Pg2SqliteOptions,
) -> Result<Option<Distinct>, crate::errors::Error> {
    distinct
        .map(|d| {
            Ok(match d {
                Distinct::On(_) => {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                        "DISTINCT ON is not supported in SQLite".to_string(),
                    ));
                }
                Distinct::Distinct => Distinct::Distinct,
                Distinct::All => Distinct::All,
            })
        })
        .transpose()
}

fn translate_named_window(
    named_windows: &[NamedWindowDefinition],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<NamedWindowDefinition>, crate::errors::Error> {
    named_windows
        .iter()
        .map(|nwd| {
            let translated_expr = match &nwd.1 {
                NamedWindowExpr::NamedWindow(ident) => NamedWindowExpr::NamedWindow(ident.clone()),
                NamedWindowExpr::WindowSpec(spec) => {
                    NamedWindowExpr::WindowSpec(translate_window_spec(spec, schema, options)?)
                }
            };
            Ok(NamedWindowDefinition(nwd.0.clone(), translated_expr))
        })
        .collect()
}

fn translate_window_spec(
    spec: &sqlparser::ast::WindowSpec,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::WindowSpec, crate::errors::Error> {
    let partition_by = spec
        .partition_by
        .iter()
        .map(|e| e.translate(schema, options))
        .collect::<Result<Vec<_>, _>>()?;
    let order_by = spec
        .order_by
        .iter()
        .map(|e| e.translate(schema, options))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(sqlparser::ast::WindowSpec {
        window_name: spec.window_name.clone(),
        partition_by,
        order_by,
        window_frame: spec.window_frame.clone(),
    })
}

fn translate_values(
    values: &Values,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Values, crate::errors::Error> {
    let translated_rows = values
        .rows
        .iter()
        .map(|row| {
            row.iter().map(|expr| expr.translate(schema, options)).collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Values {
        explicit_row: values.explicit_row,
        rows: translated_rows,
        value_keyword: values.value_keyword,
    })
}
