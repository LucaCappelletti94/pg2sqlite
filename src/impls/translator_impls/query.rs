//! Implementation of the [`Translator`] trait for the
//! `Query`, `SetExpr`, and `Select` types.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, Distinct, Expr, Fetch, Function, GroupByExpr, GroupByWithModifier, Ident,
    LimitClause, Offset, OffsetRows, OrderBy, OrderByExpr, OrderByKind, PipeOperator, Query,
    Select, SelectItem, SetExpr, SetOperator, SetQuantifier, Setting, TableAlias, TableFactor,
    TableWithJoins, Value, ValueWithSpan, WindowSpec, WindowType,
    helpers::attached_token::AttachedToken,
};

use super::helpers::{
    Forward, translate_order_by_clause, translate_pipe_operators, translate_query_settings,
    translate_with_clause,
};
use crate::{
    impls::{
        function_helpers::{integer_literal, simple_function_expr},
        query_builder::make_query,
        shared_helpers::TranslationDirection,
    },
    traits::translator::TranslatorWithContext,
    warnings::TranslationWarning,
};

pub(crate) const DISTINCT_ON_DERIVED_ALIAS: &str = "__pg2sqlite_distinct_on";
pub(crate) const DISTINCT_ON_ROWNUM_ALIAS: &str = "__pg2sqlite_rn";

crate::traits::translator::impl_contextual_translator!(Query => Query);
impl crate::traits::translator::TranslatorWithContext for Query {
    fn translate_with_warnings(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        let with = translate_with_clause(self.with.as_ref(), schema, options, emit)?;
        let order_by = translate_order_by(self.order_by.as_ref(), schema, options, emit)?;
        let (limit_clause, fetch) = forward_translate_limit_and_fetch(
            self.limit_clause.as_ref(),
            self.fetch.as_ref(),
            schema,
            options,
            emit,
        )?;
        let settings = translate_query_settings(self.settings.as_ref(), schema, options, emit)?;
        let pipe_operators = translate_pipe_operators(&self.pipe_operators, schema, options, emit)?;

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
            emit,
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
            emit,
        )? {
            return Ok(rewritten);
        }

        // Emit warning if FOR UPDATE or FOR SHARE is present
        if !self.locks.is_empty() || self.for_clause.is_some() {
            emit(TranslationWarning::LossyDrop {
                construct: "FOR UPDATE / FOR SHARE".to_string(),
                reason: "SQLite has no row-level locking; the clause is dropped".to_string(),
            });
        }

        Ok(build_query_envelope(
            self.body.translate_with_warnings(schema, options, emit)?,
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
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<OrderBy>, crate::errors::Error> {
    translate_order_by_clause(order_by, schema, options, emit)
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
/// Converts a PostgreSQL FETCH/OFFSET pair into a SQLite LIMIT/OFFSET clause.
///
/// SQLite accepts only `LIMIT m` and `LIMIT m OFFSET n`. It understands neither
/// the SQL-standard `FETCH FIRST m ROWS ONLY` form nor the `ROW`/`ROWS` keyword
/// on `OFFSET`. This function performs the following rewrites:
///
/// * `FETCH FIRST m ROWS ONLY` (no offset) -> `LIMIT m`
/// * `OFFSET n ROWS FETCH FIRST m ROWS ONLY` -> `LIMIT m OFFSET n`
/// * Bare `OFFSET n ROWS` (no FETCH, no LIMIT) -> `LIMIT -1 OFFSET n` (SQLite
///   requires a LIMIT before OFFSET; -1 means no upper bound)
/// * Existing `LIMIT m OFFSET n` is preserved, with `ROWS`/`ROW` stripped.
///
/// # Errors
///
/// Returns `Error::TranslationRefusal` for `FETCH ... WITH TIES` (no
/// SQLite equivalent) and for `FETCH FIRST ... PERCENT ROWS ONLY` (SQLite has
/// no percentage limit).
fn forward_translate_limit_and_fetch(
    limit_clause: Option<&LimitClause>,
    fetch: Option<&Fetch>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<(Option<LimitClause>, Option<Fetch>), crate::errors::Error> {
    use crate::errors::Error;

    if let Some(f) = fetch {
        if f.with_ties {
            return Err(Error::forward_refusal(
                "FETCH ... WITH TIES is not supported in SQLite. SQLite has no equivalent. \
                         Use a window function with ROW_NUMBER() to emulate it."
                    .to_string(),
            ));
        }
        if f.percent {
            return Err(Error::forward_refusal(
                "FETCH FIRST ... PERCENT ROWS is not supported in SQLite. \
                         Compute the count explicitly and pass it as LIMIT."
                    .to_string(),
            ));
        }
        // FETCH FIRST m ROWS ONLY -> LIMIT m [OFFSET n]
        let quantity = f
            .quantity
            .as_ref()
            .map(|q| Forward::translate_expr(q, schema, options, emit))
            .transpose()?;
        let offset = match limit_clause {
            Some(LimitClause::LimitOffset { offset: Some(o), .. }) => {
                Some(Offset {
                    value: Forward::translate_expr(&o.value, schema, options, emit)?,
                    rows: OffsetRows::None,
                })
            }
            _ => None,
        };
        return Ok((
            Some(LimitClause::LimitOffset {
                limit: Some(quantity.unwrap_or_else(|| integer_literal(0))),
                offset,
                limit_by: vec![],
            }),
            None,
        ));
    }

    // No FETCH clause. Normalize the existing limit clause: translate
    // expressions and strip the ROW/ROWS keyword from OFFSET, which
    // SQLite does not accept. Also add LIMIT -1 when only OFFSET is
    // present, because SQLite requires a LIMIT before OFFSET.
    let new_lc = match limit_clause {
        None => None,
        Some(LimitClause::LimitOffset { limit, offset, limit_by }) => {
            let translated_limit = limit
                .as_ref()
                .map(|e| Forward::translate_expr(e, schema, options, emit))
                .transpose()?;
            let translated_offset = offset
                .as_ref()
                .map(|o| {
                    Ok::<_, crate::errors::Error>(Offset {
                        value: Forward::translate_expr(&o.value, schema, options, emit)?,
                        rows: OffsetRows::None,
                    })
                })
                .transpose()?;
            let translated_limit_by = limit_by
                .iter()
                .map(|e| Forward::translate_expr(e, schema, options, emit))
                .collect::<Result<Vec<_>, _>>()?;
            let final_limit = if translated_limit.is_none() && translated_offset.is_some() {
                Some(integer_literal(-1))
            } else {
                translated_limit
            };
            Some(LimitClause::LimitOffset {
                limit: final_limit,
                offset: translated_offset,
                limit_by: translated_limit_by,
            })
        }
        Some(LimitClause::OffsetCommaLimit { offset, limit }) => {
            Some(LimitClause::OffsetCommaLimit {
                offset: Forward::translate_expr(offset, schema, options, emit)?,
                limit: Forward::translate_expr(limit, schema, options, emit)?,
            })
        }
    };
    Ok((new_lc, None))
}

fn ensure_distinct_on_projection_is_rewriteable(
    projection: &[SelectItem],
) -> Result<Vec<SelectItem>, crate::errors::Error> {
    let mut seen = alloc::collections::BTreeSet::new();
    let mut named_projection = Vec::with_capacity(projection.len());

    for item in projection {
        let (expr, alias) = match item {
            SelectItem::ExprWithAlias { expr, alias } => (expr.clone(), alias.clone()),
            SelectItem::UnnamedExpr(Expr::Identifier(ident)) => {
                (Expr::Identifier(ident.clone()), ident.clone())
            }
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => {
                let alias = parts.last().cloned().ok_or_else(|| {
                    crate::errors::Error::forward_refusal(
                        "DISTINCT ON projection contains an empty compound identifier".to_string(),
                    )
                })?;
                (Expr::CompoundIdentifier(parts.clone()), alias)
            }
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => {
                return Err(crate::errors::Error::forward_refusal(
                    "DISTINCT ON rewrite supports only projections that can be named explicitly. \
                 Wildcards are not supported."
                        .to_string(),
                ));
            }
            SelectItem::ExprWithAliases { .. } => {
                return Err(crate::errors::Error::forward_refusal(
                    "DISTINCT ON rewrite does not support multi-alias projections \
                 (Spark `expr AS (a, b)` form)"
                        .to_string(),
                ));
            }
            SelectItem::UnnamedExpr(other_expr) => {
                return Err(crate::errors::Error::forward_refusal(format!(
                    "DISTINCT ON rewrite supports only named/identifier projections. \
                     Unsupported projection item: {other_expr}",
                )));
            }
        };

        if !seen.insert(alias.value.to_lowercase()) {
            return Err(crate::errors::Error::forward_refusal(
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
                    Err(crate::errors::Error::forward_refusal(
                        "DISTINCT ON rewrite expected aliased projection items".to_string(),
                    ))
                }
            }
        })
        .collect()
}

fn row_number_expr(partition_by: Vec<Expr>, order_by: Vec<OrderByExpr>) -> Expr {
    simple_function_expr(
        "ROW_NUMBER",
        vec![],
        Some(WindowType::WindowSpec(WindowSpec {
            window_name: None,
            partition_by,
            order_by,
            window_frame: None,
        })),
    )
}

/// Wraps `inner` in the derived-table row number filter that stands in for
/// `DISTINCT ON`. `inner` must project exactly `aliases`, in that order.
///
/// The reverse direction rebuilds this shape from a parsed query and compares
/// the result with what it parsed, so the two directions cannot drift apart.
pub(crate) fn distinct_on_window_select(
    mut inner: Select,
    aliases: &[Ident],
    partition_by: Vec<Expr>,
    window_order: Vec<OrderByExpr>,
) -> Select {
    let flavor = inner.flavor;
    inner.projection.push(SelectItem::ExprWithAlias {
        expr: row_number_expr(partition_by, window_order),
        alias: Ident::new(DISTINCT_ON_ROWNUM_ALIAS),
    });

    let inner_query = make_query(None, SetExpr::Select(Box::new(inner)));

    let derived_alias = Ident::new(DISTINCT_ON_DERIVED_ALIAS);
    Select {
        select_token: AttachedToken::empty(),
        distinct: None,
        top: None,
        top_before_distinct: false,
        projection: aliases
            .iter()
            .map(|alias| {
                SelectItem::UnnamedExpr(Expr::CompoundIdentifier(vec![
                    derived_alias.clone(),
                    alias.clone(),
                ]))
            })
            .collect(),
        into: None,
        from: vec![TableWithJoins {
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
        }],
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
        flavor,
        exclude: None,
        optimizer_hints: Vec::new(),
        select_modifiers: None,
    }
}

fn null_literal() -> Expr {
    Expr::Value(ValueWithSpan { value: Value::Null, span: sqlparser::tokenizer::Span::empty() })
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn try_translate_distinct_on_query(
    query: &Query,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    with: Option<sqlparser::ast::With>,
    order_by: Option<OrderBy>,
    limit_clause: Option<LimitClause>,
    fetch: Option<Fetch>,
    settings: Option<Vec<Setting>>,
    pipe_operators: Vec<PipeOperator>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<Query>, crate::errors::Error> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Ok(None);
    };
    let Some(Distinct::On(distinct_on_exprs)) = select.distinct.as_ref() else {
        return Ok(None);
    };

    if select.top.is_some() {
        return Err(crate::errors::Error::forward_refusal(
            "DISTINCT ON rewrite does not support TOP clauses".to_string(),
        ));
    }

    let mut inner_select = select.as_ref().clone();
    inner_select.distinct = None;
    let mut translated_inner = inner_select.translate_with_warnings(schema, options, emit)?;

    translated_inner.projection =
        ensure_distinct_on_projection_is_rewriteable(&translated_inner.projection)?;
    let projection_aliases = projection_aliases(&translated_inner.projection)?;

    let partition_by = distinct_on_exprs
        .iter()
        .map(|expr| expr.translate_with_warnings(schema, options, emit))
        .collect::<Result<Vec<_>, _>>()?;

    // Two separate corrections, because the two ORDER BY positions see different
    // scopes and each was wrong in its own way.
    //
    // The window sits INSIDE the derived table, where the source table's columns
    // are in scope, so it keeps every operand. What it cannot carry is an output
    // alias: a column alias is invisible inside `OVER`, and SQLite answers `no
    // such column`. So each operand is resolved back to the expression the
    // projection aliased.
    //
    // The outer ORDER BY sees only what the derived table projects. PostgreSQL
    // requires the DISTINCT ON expressions to be the leftmost ORDER BY operands,
    // verified on 16, which rejects `DISTINCT ON (sensor) ... ORDER BY value`. So
    // once one row per partition survives, those leading operands already order
    // the result totally and every operand after them is unobservable. Dropping
    // them is therefore exact rather than a compromise, and it avoids projecting
    // helper columns, which would break the reverse direction: it rebuilds this
    // shape and compares it with what it parsed.
    let window_order = match &order_by {
        Some(ob) => {
            match &ob.kind {
                OrderByKind::Expressions(exprs) => {
                    exprs
                        .iter()
                        .map(|item| resolve_alias_in_order_by(item, &translated_inner.projection))
                        .collect::<Vec<_>>()
                }
                OrderByKind::All(_) => {
                    return Err(crate::errors::Error::forward_refusal(
                        "DISTINCT ON rewrite does not support ORDER BY ALL".to_string(),
                    ));
                }
            }
        }
        None => Vec::new(),
    };

    let outer_order_by = truncate_order_by_to_partition(order_by, distinct_on_exprs.len());

    let outer_select = distinct_on_window_select(
        translated_inner,
        &projection_aliases,
        partition_by,
        window_order,
    );

    // Emit warning if FOR UPDATE or FOR SHARE is present
    if !query.locks.is_empty() || query.for_clause.is_some() {
        emit(TranslationWarning::LossyDrop {
            construct: "FOR UPDATE / FOR SHARE".to_string(),
            reason: "SQLite has no row-level locking; the clause is dropped".to_string(),
        });
    }

    Ok(Some(build_query_envelope(
        SetExpr::Select(Box::new(outer_select)),
        with,
        outer_order_by,
        limit_clause,
        fetch,
        settings,
        query.format_clause.clone(),
        pipe_operators,
    )))
}

/// An `ORDER BY` operand with any output alias replaced by the expression the
/// projection aliased, so it can be used inside `OVER`, where aliases do not
/// resolve. Anything else is returned unchanged.
fn resolve_alias_in_order_by(item: &OrderByExpr, projection: &[SelectItem]) -> OrderByExpr {
    let Expr::Identifier(name) = &item.expr else {
        return item.clone();
    };
    let underlying = projection.iter().find_map(|projected| {
        match projected {
            SelectItem::ExprWithAlias { expr, alias }
                if alias.value.eq_ignore_ascii_case(&name.value) =>
            {
                Some(expr.clone())
            }
            _ => None,
        }
    });
    match underlying {
        Some(expr) => OrderByExpr { expr, ..item.clone() },
        None => item.clone(),
    }
}

/// The outer `ORDER BY` reduced to its leading `partition_len` operands, which
/// are the `DISTINCT ON` expressions PostgreSQL requires there.
fn truncate_order_by_to_partition(
    order_by: Option<OrderBy>,
    partition_len: usize,
) -> Option<OrderBy> {
    let mut order_by = order_by?;
    if let OrderByKind::Expressions(exprs) = &mut order_by.kind {
        if exprs.len() <= partition_len {
            return Some(order_by);
        }
        exprs.truncate(partition_len);
    }
    Some(order_by)
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
        return Err(crate::errors::Error::forward_refusal(
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
                    return Err(crate::errors::Error::forward_refusal(format!(
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

            Err(crate::errors::Error::forward_refusal(
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
    options: &crate::options::TranslationContext<'_>,
    with: Option<sqlparser::ast::With>,
    order_by: Option<OrderBy>,
    limit_clause: Option<LimitClause>,
    fetch: Option<Fetch>,
    settings: Option<Vec<Setting>>,
    pipe_operators: Vec<PipeOperator>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<Query>, crate::errors::Error> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Ok(None);
    };

    let GroupByExpr::Expressions(group_exprs, modifiers) = &select.group_by else {
        if matches!(&select.group_by, GroupByExpr::All(mods) if !mods.is_empty()) {
            return Err(crate::errors::Error::forward_refusal(
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
            return Err(crate::errors::Error::forward_refusal(
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
                return Err(crate::errors::Error::forward_refusal("GROUPING SETS/ROLLUP/CUBE rewrite supports at most one grouping operator per GROUP BY"
                    .to_string()));
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
        return Err(crate::errors::Error::forward_refusal(
            "GROUPING SETS/ROLLUP/CUBE rewrite does not support DISTINCT in the same SELECT"
                .to_string(),
        ));
    }
    if select.having.is_some() {
        return Err(crate::errors::Error::forward_refusal(
            "GROUPING SETS/ROLLUP/CUBE rewrite does not yet support HAVING clauses".to_string(),
        ));
    }
    if select.top.is_some() {
        return Err(crate::errors::Error::forward_refusal(
            "GROUPING SETS/ROLLUP/CUBE rewrite does not support TOP clauses".to_string(),
        ));
    }

    let mut base_select = select.as_ref().clone();
    base_select.group_by = GroupByExpr::Expressions(Vec::new(), Vec::new());
    let translated_base = base_select.translate_with_warnings(schema, options, emit)?;

    let translated_sets = expanded_sets
        .iter()
        .map(|set| {
            set.iter()
                .map(|expr| expr.translate_with_warnings(schema, options, emit))
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

    // Emit warning if FOR UPDATE or FOR SHARE is present
    if !query.locks.is_empty() || query.for_clause.is_some() {
        emit(TranslationWarning::LossyDrop {
            construct: "FOR UPDATE / FOR SHARE".to_string(),
            reason: "SQLite has no row-level locking; the clause is dropped".to_string(),
        });
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

crate::traits::translator::impl_contextual_translator!(SetExpr => SetExpr);
impl crate::traits::translator::TranslatorWithContext for SetExpr {
    fn translate_with_warnings(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        crate::impls::shared_helpers::translate_set_expr_shared::<Forward>(
            self, schema, options, emit,
        )
    }
}

crate::traits::translator::impl_contextual_translator!(Select => Select);
impl crate::traits::translator::TranslatorWithContext for Select {
    fn translate_with_warnings(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        crate::impls::shared_helpers::translate_select_shared::<Forward>(
            self, schema, options, emit,
        )
    }
}

/// Test-only wrappers for internal helpers.
#[cfg(all(test, feature = "std"))]
fn translate_group_by(
    group_by: &sqlparser::ast::GroupByExpr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<sqlparser::ast::GroupByExpr, crate::errors::Error> {
    crate::impls::shared_helpers::translate_group_by_expr::<Forward>(
        group_by,
        schema,
        options,
        &mut |_| {},
    )
}

#[cfg(all(test, feature = "std"))]
fn translate_limit_clause(
    limit_clause: Option<&LimitClause>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<LimitClause>, crate::errors::Error> {
    crate::impls::shared_helpers::translate_limit_clause::<Forward>(
        limit_clause,
        schema,
        options,
        &mut |_| {},
    )
}

#[cfg(all(test, feature = "std"))]
fn translate_fetch(
    fetch: Option<&Fetch>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<Fetch>, crate::errors::Error> {
    crate::impls::shared_helpers::translate_fetch_clause::<Forward>(
        fetch,
        schema,
        options,
        &mut |_| {},
    )
}

#[cfg(all(test, feature = "std"))]
fn translate_distinct(
    distinct: Option<&Distinct>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<Distinct>, crate::errors::Error> {
    crate::impls::shared_helpers::translate_distinct_shared::<Forward>(
        distinct,
        schema,
        options,
        &mut |_| {},
    )
}

#[cfg(all(test, feature = "std"))]
fn translate_named_window(
    named_windows: &[sqlparser::ast::NamedWindowDefinition],
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Vec<sqlparser::ast::NamedWindowDefinition>, crate::errors::Error> {
    crate::impls::shared_helpers::translate_named_windows::<Forward>(
        named_windows,
        schema,
        options,
        &mut |_| {},
    )
}

#[cfg(all(test, feature = "std"))]
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
        AGGREGATE_NAMES, GroupingRewriteKind, ensure_distinct_on_projection_is_rewriteable,
        expand_cube, is_aggregate_expression, projection_aliases,
        rewrite_projection_for_grouping_set, translate_distinct, translate_fetch,
        translate_limit_clause, translate_named_window, translate_order_by,
        try_translate_distinct_on_query, try_translate_grouping_query,
    };
    use crate::prelude::{Pg2SqliteOptions, Translator};

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap()
    }

    /// This crate keeps its PostgreSQL names in more than one place, and the
    /// reverse direction's inventories were built from the other one. That is
    /// how `jsonb_object_agg` came to be a name this list carries and the
    /// reverse direction refused, which is the same shape as the omission that
    /// prompted the inventory in the first place.
    ///
    /// So every name here has to be one some direction can place: a SQLite name
    /// an arm handles, or a name an inventory vouches for.
    #[test]
    fn every_aggregate_name_is_one_some_direction_places() {
        // PostgreSQL 17 has `range_agg` and no `multirange_agg`, measured
        // against its catalogue, so no inventory may claim it.
        const ABSENT_FROM_POSTGRES: [&str; 1] = ["multirange_agg"];

        for name in AGGREGATE_NAMES {
            if ABSENT_FROM_POSTGRES.contains(name) {
                continue;
            }
            assert!(
                {
                    let class = crate::impls::sqlite_functions::classify(name);
                    class.sqlite_builtin || class.shared_with_postgres || class.postgres_only
                },
                "{name} is an aggregate this crate knows, so the reverse direction must place it \
                 too"
            );
        }
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
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let mut distinct_on_order_all =
            parse_query("SELECT DISTINCT ON (user_id) user_id, ts FROM events ORDER BY user_id");
        distinct_on_order_all.order_by = Some(sqlparser::ast::OrderBy {
            kind: sqlparser::ast::OrderByKind::All(sqlparser::ast::OrderByOptions {
                sort: None,
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
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let order_by_all = Some(sqlparser::ast::OrderBy {
            kind: sqlparser::ast::OrderByKind::All(sqlparser::ast::OrderByOptions {
                sort: Some(sqlparser::ast::OrderBySort::Asc),
                nulls_first: Some(false),
            }),
            interpolate: None,
        });
        let _ = translate_order_by(order_by_all.as_ref(), &schema, &options, &mut |_| {}).unwrap();

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
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

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
            &mut |_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("TOP clauses"));
        additional_query_helpers_cover_grouping_error_paths();
    }
    fn additional_query_helpers_cover_grouping_error_paths() {
        let schema = empty_schema();
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
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
            &mut |_| {},
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
            &mut |_| {},
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
            &mut |_| {},
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
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

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
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

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
            &mut |_| {},
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
            &mut |_| {},
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
            &mut |_| {},
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
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
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
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let mut query = parse_query("SELECT id FROM users");
        let SetExpr::Select(select) = query.body.as_mut() else {
            panic!("expected select");
        };

        select.prewhere = Some(parse_expr("now()"));

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

    /// R122: the SELECT clauses foreign to both PostgreSQL and SQLite refuse
    /// rather than translate through into SQL SQLite cannot parse.
    #[test]
    fn foreign_select_clauses_are_refused() {
        let schema = empty_schema();
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let refused = |mutate: &dyn Fn(&mut sqlparser::ast::Select), needle: &str| {
            let mut query = parse_query("SELECT id FROM users");
            let SetExpr::Select(select) = query.body.as_mut() else {
                panic!("expected select");
            };
            mutate(select);
            let err = query
                .translate(&schema, &options)
                .expect_err("a foreign SELECT clause must refuse");
            assert!(err.to_string().contains(needle), "{needle}: {err}");
        };

        refused(
            &|select| {
                select.lateral_views = vec![sqlparser::ast::LateralView {
                    lateral_view: parse_expr("now()"),
                    lateral_view_name: sqlparser::ast::ObjectName::from(vec![
                        sqlparser::ast::Ident::new("v"),
                    ]),
                    lateral_col_alias: vec![sqlparser::ast::Ident::new("c")],
                    outer: false,
                }];
            },
            "LATERAL VIEW",
        );
        refused(&|select| select.cluster_by = vec![parse_expr("now()")], "CLUSTER BY");
        refused(&|select| select.distribute_by = vec![parse_expr("now()")], "DISTRIBUTE BY");
        refused(
            &|select| {
                select.sort_by = vec![sqlparser::ast::OrderByExpr {
                    expr: parse_expr("now()"),
                    options: sqlparser::ast::OrderByOptions { sort: None, nulls_first: None },
                    with_fill: None,
                }];
            },
            "SORT BY",
        );
        refused(&|select| select.qualify = Some(parse_expr("id = 1")), "QUALIFY");
        refused(
            &|select| {
                select.connect_by = vec![sqlparser::ast::ConnectByKind::StartWith {
                    start_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                    condition: Box::new(parse_expr("now()")),
                }];
            },
            "CONNECT BY",
        );
    }
}
