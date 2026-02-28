//! Shared helper functions for translating table references, joins, and select
//! items. Generic over translation direction (forward or reverse).

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    Assignment, ConnectByKind, Expr, ExprWithAlias, ExprWithAliasAndOrderBy, Fetch, FromTable,
    FunctionArg, FunctionArgExpr, FunctionArgumentClause, FunctionArgumentList, FunctionArguments,
    GroupByExpr, HavingBound, Join, JoinConstraint, JoinOperator, LateralView, LimitClause,
    ListAggOnOverflow, Measure, NamedWindowDefinition, NamedWindowExpr, ObjectName, ObjectNamePart,
    OrderBy, OrderByExpr, OrderByKind, PipeOperator, PivotValueSource, Query, SelectItem, Setting,
    Statement, SymbolDefinition, TableFactor, TableFunctionArgs, TableSample, TableSampleBucket,
    TableSampleKind, TableSampleQuantity, TableVersion, TableWithJoins, UpdateTableFromKind,
    Values, WindowFrame, WindowFrameBound, WindowSpec, WindowType, With, WithFill,
    XmlNamespaceDefinition, XmlPassingArgument, XmlPassingClause, XmlTableColumn,
    XmlTableColumnOption,
};

use crate::{errors::Error, prelude::Pg2SqliteOptions};

/// Abstracts the direction of translation so that shared helper functions
/// can work for both forward (`Translator`) and reverse (`ReverseTranslator`)
/// translation.
pub(crate) trait TranslationDirection {
    /// `true` for forward (PostgreSQL → SQLite) translation, `false` for
    /// reverse.
    const IS_FORWARD: bool = false;

    fn translate_expr(
        expr: &Expr,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Expr, Error>;
    fn translate_query(
        query: &Query,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Query, Error>;
    fn translate_insert(
        insert: &sqlparser::ast::Insert,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<sqlparser::ast::Insert, Error>;
    fn translate_delete(
        delete: &sqlparser::ast::Delete,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<sqlparser::ast::Delete, Error>;

    fn translate_object_name(
        name: &ObjectName,
        _schema: &ParserDB,
        _options: &Pg2SqliteOptions,
    ) -> Result<ObjectName, Error> {
        Ok(name.clone())
    }
}

/// Shared unsupported-feature message for `generate_series` usage.
pub(crate) const GENERATE_SERIES_UNSUPPORTED_MESSAGE: &str = "generate_series() is not available in standard SQLite. \
     Use a recursive CTE instead: \
     WITH RECURSIVE s(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM s WHERE n < N) SELECT n FROM s";

/// Returns `true` when an object name resolves to `generate_series`.
#[must_use]
pub(crate) fn is_generate_series_object_name(name: &ObjectName) -> bool {
    name.0
        .last()
        .and_then(|part| {
            if let ObjectNamePart::Identifier(id) = part { Some(id.value.as_str()) } else { None }
        })
        .is_some_and(|value| value.eq_ignore_ascii_case("generate_series"))
}

/// Returns the standardized error for unsupported `generate_series`.
#[must_use]
pub(crate) fn generate_series_not_supported_error() -> Error {
    Error::UnsupportedSQLiteFeature(GENERATE_SERIES_UNSUPPORTED_MESSAGE.to_string())
}

/// Extracts a stable-ish variant name from debug output.
#[must_use]
pub(crate) fn debug_variant_name(value: &impl std::fmt::Debug) -> String {
    let debug = format!("{value:?}");
    debug.split(['(', '{', ' ']).next().unwrap_or("Unknown").to_string()
}

/// Returns a normalized statement variant name for user-facing errors.
#[must_use]
pub(crate) fn statement_variant_name(statement: &Statement) -> String {
    debug_variant_name(statement)
}

/// Returns a normalized expression variant name for user-facing errors.
#[must_use]
pub(crate) fn expr_variant_name(expr: &Expr) -> String {
    debug_variant_name(expr)
}

/// Returns the expression for argument variants that carry one.
#[must_use]
pub(crate) fn function_arg_expr(arg: &FunctionArg) -> Option<&Expr> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))
        | FunctionArg::Named { arg: FunctionArgExpr::Expr(expr), .. }
        | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(expr), .. } => Some(expr),
        _ => None,
    }
}

/// Collects expression payloads from function arguments.
#[must_use]
pub(crate) fn function_argument_exprs(args: &FunctionArguments) -> Vec<&Expr> {
    match args {
        FunctionArguments::List(list) => list.args.iter().filter_map(function_arg_expr).collect(),
        _ => Vec::new(),
    }
}

/// Translates all function arguments, recursively translating any expression or
/// subquery payloads.
pub(crate) fn translate_function_arguments<D: TranslationDirection>(
    args: &FunctionArguments,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<FunctionArguments, Error> {
    match args {
        FunctionArguments::None => Ok(FunctionArguments::None),
        FunctionArguments::Subquery(query) => {
            Ok(FunctionArguments::Subquery(Box::new(D::translate_query(query, schema, options)?)))
        }
        FunctionArguments::List(list) => {
            let translated = list
                .args
                .iter()
                .map(|arg| translate_function_arg::<D>(arg, schema, options))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: list.duplicate_treatment,
                args: translated,
                clauses: translate_function_argument_clauses::<D>(&list.clauses, schema, options)?,
            }))
        }
    }
}

fn translate_function_arg_expr<D: TranslationDirection>(
    arg: &FunctionArgExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<FunctionArgExpr, Error> {
    Ok(match arg {
        FunctionArgExpr::Expr(expr) => {
            FunctionArgExpr::Expr(D::translate_expr(expr, schema, options)?)
        }
        FunctionArgExpr::QualifiedWildcard(name) => {
            FunctionArgExpr::QualifiedWildcard(name.clone())
        }
        FunctionArgExpr::Wildcard => FunctionArgExpr::Wildcard,
    })
}

fn translate_function_arg<D: TranslationDirection>(
    arg: &FunctionArg,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<FunctionArg, Error> {
    Ok(match arg {
        FunctionArg::Named { name, arg, operator } => {
            FunctionArg::Named {
                name: name.clone(),
                arg: translate_function_arg_expr::<D>(arg, schema, options)?,
                operator: operator.clone(),
            }
        }
        FunctionArg::ExprNamed { name, arg, operator } => {
            FunctionArg::ExprNamed {
                name: D::translate_expr(name, schema, options)?,
                arg: translate_function_arg_expr::<D>(arg, schema, options)?,
                operator: operator.clone(),
            }
        }
        FunctionArg::Unnamed(arg) => {
            FunctionArg::Unnamed(translate_function_arg_expr::<D>(arg, schema, options)?)
        }
    })
}

pub(crate) fn translate_setting<D: TranslationDirection>(
    setting: &Setting,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Setting, Error> {
    Ok(Setting {
        key: setting.key.clone(),
        value: D::translate_expr(&setting.value, schema, options)?,
    })
}

fn translate_table_function_args<D: TranslationDirection>(
    args: &TableFunctionArgs,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableFunctionArgs, Error> {
    Ok(TableFunctionArgs {
        args: args
            .args
            .iter()
            .map(|arg| translate_function_arg::<D>(arg, schema, options))
            .collect::<Result<Vec<_>, _>>()?,
        settings: args
            .settings
            .as_ref()
            .map(|settings| {
                settings
                    .iter()
                    .map(|setting| translate_setting::<D>(setting, schema, options))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?,
    })
}

fn translate_table_version<D: TranslationDirection>(
    version: &TableVersion,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableVersion, Error> {
    Ok(match version {
        TableVersion::ForSystemTimeAsOf(expr) => {
            TableVersion::ForSystemTimeAsOf(D::translate_expr(expr, schema, options)?)
        }
        TableVersion::TimestampAsOf(expr) => {
            TableVersion::TimestampAsOf(D::translate_expr(expr, schema, options)?)
        }
        TableVersion::VersionAsOf(expr) => {
            TableVersion::VersionAsOf(D::translate_expr(expr, schema, options)?)
        }
        TableVersion::Function(expr) => {
            TableVersion::Function(D::translate_expr(expr, schema, options)?)
        }
    })
}

fn translate_table_sample_quantity<D: TranslationDirection>(
    quantity: &TableSampleQuantity,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableSampleQuantity, Error> {
    Ok(TableSampleQuantity {
        parenthesized: quantity.parenthesized,
        value: D::translate_expr(&quantity.value, schema, options)?,
        unit: quantity.unit,
    })
}

fn translate_table_sample_bucket<D: TranslationDirection>(
    bucket: &TableSampleBucket,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableSampleBucket, Error> {
    Ok(TableSampleBucket {
        bucket: bucket.bucket.clone(),
        total: bucket.total.clone(),
        on: bucket.on.as_ref().map(|expr| D::translate_expr(expr, schema, options)).transpose()?,
    })
}

fn translate_table_sample<D: TranslationDirection>(
    sample: &TableSample,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableSample, Error> {
    Ok(TableSample {
        modifier: sample.modifier,
        name: sample.name,
        quantity: sample
            .quantity
            .as_ref()
            .map(|quantity| translate_table_sample_quantity::<D>(quantity, schema, options))
            .transpose()?,
        seed: sample.seed.clone(),
        bucket: sample
            .bucket
            .as_ref()
            .map(|bucket| translate_table_sample_bucket::<D>(bucket, schema, options))
            .transpose()?,
        offset: sample
            .offset
            .as_ref()
            .map(|expr| D::translate_expr(expr, schema, options))
            .transpose()?,
    })
}

fn translate_table_sample_kind<D: TranslationDirection>(
    sample: &TableSampleKind,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableSampleKind, Error> {
    Ok(match sample {
        TableSampleKind::BeforeTableAlias(sample) => {
            TableSampleKind::BeforeTableAlias(Box::new(translate_table_sample::<D>(
                sample, schema, options,
            )?))
        }
        TableSampleKind::AfterTableAlias(sample) => {
            TableSampleKind::AfterTableAlias(Box::new(translate_table_sample::<D>(
                sample, schema, options,
            )?))
        }
    })
}

fn translate_with_fill<D: TranslationDirection>(
    with_fill: &WithFill,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<WithFill, Error> {
    Ok(WithFill {
        from: with_fill
            .from
            .as_ref()
            .map(|expr| D::translate_expr(expr, schema, options))
            .transpose()?,
        to: with_fill
            .to
            .as_ref()
            .map(|expr| D::translate_expr(expr, schema, options))
            .transpose()?,
        step: with_fill
            .step
            .as_ref()
            .map(|expr| D::translate_expr(expr, schema, options))
            .transpose()?,
    })
}

pub(crate) fn translate_order_by_expr<D: TranslationDirection>(
    order_by_expr: &OrderByExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<OrderByExpr, Error> {
    Ok(OrderByExpr {
        expr: D::translate_expr(&order_by_expr.expr, schema, options)?,
        options: sqlparser::ast::OrderByOptions {
            asc: order_by_expr.options.asc,
            // Strip NULLS FIRST/LAST in forward direction: SQLite < 3.30 rejects
            // this syntax and the semantics differ from PostgreSQL defaults anyway.
            // Preserve it in reverse translation to maintain PostgreSQL round-trip.
            nulls_first: if D::IS_FORWARD { None } else { order_by_expr.options.nulls_first },
        },
        with_fill: order_by_expr
            .with_fill
            .as_ref()
            .map(|with_fill| translate_with_fill::<D>(with_fill, schema, options))
            .transpose()?,
    })
}

fn translate_expr_with_alias<D: TranslationDirection>(
    expr_with_alias: &ExprWithAlias,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<ExprWithAlias, Error> {
    Ok(ExprWithAlias {
        expr: D::translate_expr(&expr_with_alias.expr, schema, options)?,
        alias: expr_with_alias.alias.clone(),
    })
}

fn translate_pivot_value_source<D: TranslationDirection>(
    value_source: &PivotValueSource,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<PivotValueSource, Error> {
    Ok(match value_source {
        PivotValueSource::List(values) => {
            PivotValueSource::List(
                values
                    .iter()
                    .map(|value| translate_expr_with_alias::<D>(value, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        PivotValueSource::Any(order_by) => {
            PivotValueSource::Any(
                order_by
                    .iter()
                    .map(|order_by_expr| {
                        translate_order_by_expr::<D>(order_by_expr, schema, options)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        PivotValueSource::Subquery(query) => {
            PivotValueSource::Subquery(Box::new(D::translate_query(query, schema, options)?))
        }
    })
}

fn translate_expr_with_alias_and_order_by<D: TranslationDirection>(
    expr_with_alias_and_order_by: &ExprWithAliasAndOrderBy,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<ExprWithAliasAndOrderBy, Error> {
    Ok(ExprWithAliasAndOrderBy {
        expr: translate_expr_with_alias::<D>(&expr_with_alias_and_order_by.expr, schema, options)?,
        order_by: expr_with_alias_and_order_by.order_by,
    })
}

fn translate_assignment<D: TranslationDirection>(
    assignment: &Assignment,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Assignment, Error> {
    Ok(Assignment {
        target: assignment.target.clone(),
        value: D::translate_expr(&assignment.value, schema, options)?,
    })
}

#[allow(clippy::too_many_lines)]
fn translate_pipe_operator<D: TranslationDirection>(
    pipe_operator: &PipeOperator,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<PipeOperator, Error> {
    Ok(match pipe_operator {
        PipeOperator::Limit { expr, offset } => {
            PipeOperator::Limit {
                expr: D::translate_expr(expr, schema, options)?,
                offset: offset
                    .as_ref()
                    .map(|expr| D::translate_expr(expr, schema, options))
                    .transpose()?,
            }
        }
        PipeOperator::Where { expr } => {
            PipeOperator::Where { expr: D::translate_expr(expr, schema, options)? }
        }
        PipeOperator::OrderBy { exprs } => {
            PipeOperator::OrderBy {
                exprs: exprs
                    .iter()
                    .map(|expr| translate_order_by_expr::<D>(expr, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Select { exprs } => {
            PipeOperator::Select {
                exprs: exprs
                    .iter()
                    .map(|expr| translate_select_item::<D>(expr, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Extend { exprs } => {
            PipeOperator::Extend {
                exprs: exprs
                    .iter()
                    .map(|expr| translate_select_item::<D>(expr, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Set { assignments } => {
            PipeOperator::Set {
                assignments: assignments
                    .iter()
                    .map(|assignment| translate_assignment::<D>(assignment, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Drop { columns } => PipeOperator::Drop { columns: columns.clone() },
        PipeOperator::As { alias } => PipeOperator::As { alias: alias.clone() },
        PipeOperator::Aggregate { full_table_exprs, group_by_expr } => {
            PipeOperator::Aggregate {
                full_table_exprs: full_table_exprs
                    .iter()
                    .map(|expr| translate_expr_with_alias_and_order_by::<D>(expr, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                group_by_expr: group_by_expr
                    .iter()
                    .map(|expr| translate_expr_with_alias_and_order_by::<D>(expr, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::TableSample { sample } => {
            PipeOperator::TableSample {
                sample: Box::new(translate_table_sample::<D>(sample.as_ref(), schema, options)?),
            }
        }
        PipeOperator::Rename { mappings } => PipeOperator::Rename { mappings: mappings.clone() },
        PipeOperator::Union { set_quantifier, queries } => {
            PipeOperator::Union {
                set_quantifier: *set_quantifier,
                queries: queries
                    .iter()
                    .map(|query| D::translate_query(query, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Intersect { set_quantifier, queries } => {
            PipeOperator::Intersect {
                set_quantifier: *set_quantifier,
                queries: queries
                    .iter()
                    .map(|query| D::translate_query(query, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Except { set_quantifier, queries } => {
            PipeOperator::Except {
                set_quantifier: *set_quantifier,
                queries: queries
                    .iter()
                    .map(|query| D::translate_query(query, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Call { function, alias } => {
            let translated_expr =
                D::translate_expr(&Expr::Function(function.clone()), schema, options)?;
            let Expr::Function(translated_function) = translated_expr else {
                return Err(Error::UnsupportedSQLiteFeature(format!(
                    "Pipe CALL translation expected function expression, got {}",
                    expr_variant_name(&translated_expr)
                )));
            };
            PipeOperator::Call { function: translated_function, alias: alias.clone() }
        }
        PipeOperator::Pivot { aggregate_functions, value_column, value_source, alias } => {
            PipeOperator::Pivot {
                aggregate_functions: aggregate_functions
                    .iter()
                    .map(|expr| translate_expr_with_alias::<D>(expr, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                value_column: value_column.clone(),
                value_source: translate_pivot_value_source::<D>(value_source, schema, options)?,
                alias: alias.clone(),
            }
        }
        PipeOperator::Unpivot { value_column, name_column, unpivot_columns, alias } => {
            PipeOperator::Unpivot {
                value_column: value_column.clone(),
                name_column: name_column.clone(),
                unpivot_columns: unpivot_columns.clone(),
                alias: alias.clone(),
            }
        }
        PipeOperator::Join(join) => PipeOperator::Join(translate_join::<D>(join, schema, options)?),
    })
}

pub(crate) fn translate_connect_by_kinds<D: TranslationDirection>(
    connect_by_kinds: &[ConnectByKind],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<ConnectByKind>, Error> {
    connect_by_kinds
        .iter()
        .map(|connect_by| {
            Ok(match connect_by {
                ConnectByKind::ConnectBy { connect_token, nocycle, relationships } => {
                    ConnectByKind::ConnectBy {
                        connect_token: connect_token.clone(),
                        nocycle: *nocycle,
                        relationships: relationships
                            .iter()
                            .map(|expr| D::translate_expr(expr, schema, options))
                            .collect::<Result<Vec<_>, _>>()?,
                    }
                }
                ConnectByKind::StartWith { start_token, condition } => {
                    ConnectByKind::StartWith {
                        start_token: start_token.clone(),
                        condition: Box::new(D::translate_expr(condition, schema, options)?),
                    }
                }
            })
        })
        .collect()
}

pub(crate) fn translate_query_settings<D: TranslationDirection>(
    settings: Option<&Vec<Setting>>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Vec<Setting>>, Error> {
    settings
        .map(|settings| {
            settings
                .iter()
                .map(|setting| translate_setting::<D>(setting, schema, options))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()
}

pub(crate) fn translate_with_clause<D: TranslationDirection>(
    with: Option<&With>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<With>, Error> {
    with.map(|w| {
        let cte_tables = w
            .cte_tables
            .iter()
            .map(|cte| {
                Ok(sqlparser::ast::Cte {
                    alias: cte.alias.clone(),
                    query: Box::new(D::translate_query(&cte.query, schema, options)?),
                    from: cte.from.clone(),
                    materialized: cte.materialized,
                    closing_paren_token: cte.closing_paren_token.clone(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(With { with_token: w.with_token.clone(), recursive: w.recursive, cte_tables })
    })
    .transpose()
}

pub(crate) fn translate_order_by_clause<D: TranslationDirection>(
    order_by: Option<&OrderBy>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<OrderBy>, Error> {
    order_by
        .map(|ob| -> Result<OrderBy, Error> {
            let kind = match &ob.kind {
                OrderByKind::Expressions(exprs) => {
                    OrderByKind::Expressions(
                        exprs
                            .iter()
                            .map(|expr| translate_order_by_expr::<D>(expr, schema, options))
                            .collect::<Result<Vec<_>, _>>()?,
                    )
                }
                OrderByKind::All(all) => OrderByKind::All(*all),
            };
            Ok(OrderBy { kind, interpolate: ob.interpolate.clone() })
        })
        .transpose()
}

pub(crate) fn translate_limit_clause<D: TranslationDirection>(
    limit_clause: Option<&LimitClause>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<LimitClause>, Error> {
    limit_clause
        .map(|lc| {
            Ok(match lc {
                LimitClause::LimitOffset { limit, offset, limit_by } => {
                    LimitClause::LimitOffset {
                        limit: limit
                            .as_ref()
                            .map(|e| D::translate_expr(e, schema, options))
                            .transpose()?,
                        offset: offset
                            .as_ref()
                            .map(|o| {
                                Ok::<_, Error>(sqlparser::ast::Offset {
                                    value: D::translate_expr(&o.value, schema, options)?,
                                    rows: o.rows,
                                })
                            })
                            .transpose()?,
                        limit_by: limit_by
                            .iter()
                            .map(|e| D::translate_expr(e, schema, options))
                            .collect::<Result<Vec<_>, _>>()?,
                    }
                }
                LimitClause::OffsetCommaLimit { offset, limit } => {
                    LimitClause::OffsetCommaLimit {
                        offset: D::translate_expr(offset, schema, options)?,
                        limit: D::translate_expr(limit, schema, options)?,
                    }
                }
            })
        })
        .transpose()
}

pub(crate) fn translate_fetch_clause<D: TranslationDirection>(
    fetch: Option<&Fetch>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Fetch>, Error> {
    fetch
        .map(|f| {
            Ok(Fetch {
                with_ties: f.with_ties,
                percent: f.percent,
                quantity: f
                    .quantity
                    .as_ref()
                    .map(|e| D::translate_expr(e, schema, options))
                    .transpose()?,
            })
        })
        .transpose()
}

pub(crate) fn translate_group_by_expr<D: TranslationDirection>(
    group_by: &GroupByExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<GroupByExpr, Error> {
    Ok(match group_by {
        GroupByExpr::Expressions(exprs, modifiers) => {
            GroupByExpr::Expressions(
                exprs
                    .iter()
                    .map(|e| D::translate_expr(e, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                modifiers.clone(),
            )
        }
        GroupByExpr::All(all) => GroupByExpr::All(all.clone()),
    })
}

pub(crate) fn translate_window_spec<D: TranslationDirection>(
    spec: &WindowSpec,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<WindowSpec, Error> {
    Ok(WindowSpec {
        window_name: spec.window_name.clone(),
        partition_by: spec
            .partition_by
            .iter()
            .map(|e| D::translate_expr(e, schema, options))
            .collect::<Result<Vec<_>, _>>()?,
        order_by: spec
            .order_by
            .iter()
            .map(|e| translate_order_by_expr::<D>(e, schema, options))
            .collect::<Result<Vec<_>, _>>()?,
        window_frame: spec
            .window_frame
            .as_ref()
            .map(|frame| translate_window_frame::<D>(frame, schema, options))
            .transpose()?,
    })
}

pub(crate) fn translate_window_type<D: TranslationDirection>(
    over: Option<&WindowType>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<WindowType>, Error> {
    match over {
        None => Ok(None),
        Some(WindowType::NamedWindow(name)) => Ok(Some(WindowType::NamedWindow(name.clone()))),
        Some(WindowType::WindowSpec(spec)) => {
            Ok(Some(WindowType::WindowSpec(translate_window_spec::<D>(spec, schema, options)?)))
        }
    }
}

fn translate_window_frame_bound<D: TranslationDirection>(
    bound: &WindowFrameBound,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<WindowFrameBound, Error> {
    Ok(match bound {
        WindowFrameBound::Preceding(Some(e)) => {
            WindowFrameBound::Preceding(Some(Box::new(D::translate_expr(e, schema, options)?)))
        }
        WindowFrameBound::Following(Some(e)) => {
            WindowFrameBound::Following(Some(Box::new(D::translate_expr(e, schema, options)?)))
        }
        other => other.clone(),
    })
}

fn translate_window_frame<D: TranslationDirection>(
    frame: &WindowFrame,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<WindowFrame, Error> {
    Ok(WindowFrame {
        units: frame.units,
        start_bound: translate_window_frame_bound::<D>(&frame.start_bound, schema, options)?,
        end_bound: frame
            .end_bound
            .as_ref()
            .map(|b| translate_window_frame_bound::<D>(b, schema, options))
            .transpose()?,
    })
}

/// Translate all [`FunctionArgumentClause`] items, recursively translating
/// any [`Expr`] payloads they contain.
pub(crate) fn translate_function_argument_clauses<D: TranslationDirection>(
    clauses: &[FunctionArgumentClause],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<FunctionArgumentClause>, Error> {
    clauses
        .iter()
        .map(|clause| translate_function_argument_clause::<D>(clause, schema, options))
        .collect()
}

fn translate_function_argument_clause<D: TranslationDirection>(
    clause: &FunctionArgumentClause,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<FunctionArgumentClause, Error> {
    Ok(match clause {
        FunctionArgumentClause::OrderBy(order_by_exprs) => {
            FunctionArgumentClause::OrderBy(
                order_by_exprs
                    .iter()
                    .map(|e| translate_order_by_expr::<D>(e, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        FunctionArgumentClause::Limit(e) => {
            FunctionArgumentClause::Limit(D::translate_expr(e, schema, options)?)
        }
        FunctionArgumentClause::Having(HavingBound(kind, e)) => {
            FunctionArgumentClause::Having(HavingBound(
                *kind,
                D::translate_expr(e, schema, options)?,
            ))
        }
        FunctionArgumentClause::OnOverflow(ListAggOnOverflow::Truncate { filler, with_count }) => {
            FunctionArgumentClause::OnOverflow(ListAggOnOverflow::Truncate {
                filler: filler
                    .as_ref()
                    .map(|e| D::translate_expr(e, schema, options).map(Box::new))
                    .transpose()?,
                with_count: *with_count,
            })
        }
        other => other.clone(),
    })
}

/// Translate a [`LateralView`], recursively translating its `lateral_view`
/// expression.
pub(crate) fn translate_lateral_view<D: TranslationDirection>(
    lv: &LateralView,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<LateralView, Error> {
    Ok(LateralView {
        lateral_view: D::translate_expr(&lv.lateral_view, schema, options)?,
        lateral_view_name: lv.lateral_view_name.clone(),
        lateral_col_alias: lv.lateral_col_alias.clone(),
        outer: lv.outer,
    })
}

pub(crate) fn translate_named_windows<D: TranslationDirection>(
    named_windows: &[NamedWindowDefinition],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<NamedWindowDefinition>, Error> {
    named_windows
        .iter()
        .map(|nwd| {
            let translated_expr = match &nwd.1 {
                NamedWindowExpr::NamedWindow(ident) => NamedWindowExpr::NamedWindow(ident.clone()),
                NamedWindowExpr::WindowSpec(spec) => {
                    NamedWindowExpr::WindowSpec(translate_window_spec::<D>(spec, schema, options)?)
                }
            };
            Ok(NamedWindowDefinition(nwd.0.clone(), translated_expr))
        })
        .collect()
}

pub(crate) fn translate_values_rows<D: TranslationDirection>(
    values: &Values,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Values, Error> {
    Ok(Values {
        explicit_row: values.explicit_row,
        rows: values
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|expr| D::translate_expr(expr, schema, options))
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?,
        value_keyword: values.value_keyword,
    })
}

/// Maps all tables in a [`FromTable`] using a caller-provided mapper.
pub(crate) fn map_from_table<E, F>(from: &FromTable, mut mapper: F) -> Result<FromTable, E>
where
    F: FnMut(&TableWithJoins) -> Result<TableWithJoins, E>,
{
    match from {
        FromTable::WithFromKeyword(tables) => {
            Ok(FromTable::WithFromKeyword(
                tables.iter().map(&mut mapper).collect::<Result<Vec<_>, _>>()?,
            ))
        }
        FromTable::WithoutKeyword(tables) => {
            Ok(FromTable::WithoutKeyword(
                tables.iter().map(&mut mapper).collect::<Result<Vec<_>, _>>()?,
            ))
        }
    }
}

/// Maps all table lists in an [`UpdateTableFromKind`] using a caller-provided
/// mapper.
pub(crate) fn map_update_table_from_kind<E, F>(
    from: &UpdateTableFromKind,
    mut mapper: F,
) -> Result<UpdateTableFromKind, E>
where
    F: FnMut(&TableWithJoins) -> Result<TableWithJoins, E>,
{
    match from {
        UpdateTableFromKind::BeforeSet(tables) => {
            Ok(UpdateTableFromKind::BeforeSet(
                tables.iter().map(&mut mapper).collect::<Result<Vec<_>, _>>()?,
            ))
        }
        UpdateTableFromKind::AfterSet(tables) => {
            Ok(UpdateTableFromKind::AfterSet(
                tables.iter().map(&mut mapper).collect::<Result<Vec<_>, _>>()?,
            ))
        }
    }
}

/// Shared UPDATE translation used by both forward and reverse paths.
///
/// Forward-specific: validates that no joins exist on the target table.
pub(crate) fn translate_update<D: TranslationDirection>(
    update: &sqlparser::ast::Update,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::Update, Error> {
    if D::IS_FORWARD && !update.table.joins.is_empty() {
        return Err(Error::UnsupportedSQLiteFeature(
            "UPDATE with joins on the target table is not supported in SQLite. \
             Use UPDATE ... FROM ... instead."
                .to_string(),
        ));
    }

    let assignments = update
        .assignments
        .iter()
        .map(|a| {
            Ok(Assignment {
                target: a.target.clone(),
                value: D::translate_expr(&a.value, schema, options)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;

    let selection = update
        .selection
        .as_ref()
        .map(|expr| D::translate_expr(expr, schema, options))
        .transpose()?;

    let from = update
        .from
        .as_ref()
        .map(|f| {
            map_update_table_from_kind(f, |table| {
                translate_table_with_joins::<D>(table, schema, options)
            })
        })
        .transpose()?;

    let returning = translate_returning::<D>(update.returning.as_ref(), schema, options)?;
    let limit =
        update.limit.as_ref().map(|expr| D::translate_expr(expr, schema, options)).transpose()?;

    Ok(sqlparser::ast::Update {
        update_token: update.update_token.clone(),
        optimizer_hint: update.optimizer_hint.clone(),
        table: translate_table_with_joins::<D>(&update.table, schema, options)?,
        assignments,
        from,
        selection,
        returning,
        or: update.or,
        limit,
    })
}

/// Shared DISTINCT translation.
///
/// Forward: errors on `DISTINCT ON` (not supported in SQLite).
/// Reverse: translates `DISTINCT ON` expressions.
pub(crate) fn translate_distinct_shared<D: TranslationDirection>(
    distinct: Option<&sqlparser::ast::Distinct>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<sqlparser::ast::Distinct>, Error> {
    distinct
        .map(|d| {
            Ok(match d {
                sqlparser::ast::Distinct::On(exprs) => {
                    if D::IS_FORWARD {
                        return Err(Error::UnsupportedSQLiteFeature(
                            "DISTINCT ON is not supported in SQLite".to_string(),
                        ));
                    }
                    sqlparser::ast::Distinct::On(
                        exprs
                            .iter()
                            .map(|e| D::translate_expr(e, schema, options))
                            .collect::<Result<Vec<_>, _>>()?,
                    )
                }
                sqlparser::ast::Distinct::Distinct => sqlparser::ast::Distinct::Distinct,
                sqlparser::ast::Distinct::All => sqlparser::ast::Distinct::All,
            })
        })
        .transpose()
}

/// Shared TOP quantity translation.
///
/// Forward: clones as-is. Reverse: translates quantity expressions.
pub(crate) fn translate_top_shared<D: TranslationDirection>(
    top: Option<&sqlparser::ast::Top>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<sqlparser::ast::Top>, Error> {
    top.map(|t| {
        if D::IS_FORWARD {
            return Ok(t.clone());
        }
        let quantity = t
            .quantity
            .as_ref()
            .map(|q| -> Result<sqlparser::ast::TopQuantity, Error> {
                match q {
                    sqlparser::ast::TopQuantity::Expr(expr) => {
                        Ok(sqlparser::ast::TopQuantity::Expr(D::translate_expr(
                            expr, schema, options,
                        )?))
                    }
                    sqlparser::ast::TopQuantity::Constant(c) => {
                        Ok(sqlparser::ast::TopQuantity::Constant(*c))
                    }
                }
            })
            .transpose()?;
        Ok(sqlparser::ast::Top { with_ties: t.with_ties, percent: t.percent, quantity })
    })
    .transpose()
}

/// Shared SELECT translation used by both forward and reverse paths.
pub(crate) fn translate_select_shared<D: TranslationDirection>(
    select: &sqlparser::ast::Select,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::Select, Error> {
    let selection = select
        .selection
        .as_ref()
        .map(|expr| D::translate_expr(expr, schema, options))
        .transpose()?;
    let having =
        select.having.as_ref().map(|expr| D::translate_expr(expr, schema, options)).transpose()?;
    let from = select
        .from
        .iter()
        .map(|twj| translate_table_with_joins::<D>(twj, schema, options))
        .collect::<Result<Vec<_>, _>>()?;
    let projection = select
        .projection
        .iter()
        .map(|item| translate_select_item::<D>(item, schema, options))
        .collect::<Result<Vec<_>, _>>()?;
    let prewhere = select
        .prewhere
        .as_ref()
        .map(|expr| D::translate_expr(expr, schema, options))
        .transpose()?;
    let cluster_by = select
        .cluster_by
        .iter()
        .map(|expr| D::translate_expr(expr, schema, options))
        .collect::<Result<Vec<_>, _>>()?;
    let distribute_by = select
        .distribute_by
        .iter()
        .map(|expr| D::translate_expr(expr, schema, options))
        .collect::<Result<Vec<_>, _>>()?;
    let sort_by = select
        .sort_by
        .iter()
        .map(|expr| translate_order_by_expr::<D>(expr, schema, options))
        .collect::<Result<Vec<_>, _>>()?;
    let connect_by = translate_connect_by_kinds::<D>(&select.connect_by, schema, options)?;

    Ok(sqlparser::ast::Select {
        select_token: select.select_token.clone(),
        distinct: translate_distinct_shared::<D>(select.distinct.as_ref(), schema, options)?,
        top: translate_top_shared::<D>(select.top.as_ref(), schema, options)?,
        top_before_distinct: select.top_before_distinct,
        projection,
        into: select.into.clone(),
        from,
        lateral_views: select
            .lateral_views
            .iter()
            .map(|lv| translate_lateral_view::<D>(lv, schema, options))
            .collect::<Result<Vec<_>, _>>()?,
        prewhere,
        selection,
        group_by: translate_group_by_expr::<D>(&select.group_by, schema, options)?,
        cluster_by,
        distribute_by,
        sort_by,
        having,
        named_window: translate_named_windows::<D>(&select.named_window, schema, options)?,
        qualify: select
            .qualify
            .as_ref()
            .map(|e| D::translate_expr(e, schema, options))
            .transpose()?,
        window_before_qualify: select.window_before_qualify,
        value_table_mode: select.value_table_mode,
        connect_by,
        flavor: select.flavor,
        exclude: select.exclude.clone(),
        optimizer_hint: select.optimizer_hint.clone(),
        select_modifiers: select.select_modifiers.clone(),
    })
}

/// Shared `SetExpr` translation used by both forward and reverse paths.
///
/// Forward-specific: errors on `Table` and `Merge` variants.
pub(crate) fn translate_set_expr_shared<D: TranslationDirection>(
    set_expr: &sqlparser::ast::SetExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::SetExpr, Error> {
    use sqlparser::ast::SetExpr;
    Ok(match set_expr {
        SetExpr::Select(select) => {
            SetExpr::Select(Box::new(translate_select_shared::<D>(select, schema, options)?))
        }
        SetExpr::Query(query) => {
            SetExpr::Query(Box::new(translate_query_shared::<D>(query, schema, options)?))
        }
        SetExpr::SetOperation { op, set_quantifier, left, right } => {
            SetExpr::SetOperation {
                op: *op,
                set_quantifier: *set_quantifier,
                left: Box::new(translate_set_expr_shared::<D>(left, schema, options)?),
                right: Box::new(translate_set_expr_shared::<D>(right, schema, options)?),
            }
        }
        SetExpr::Values(values) => {
            SetExpr::Values(translate_values_rows::<D>(values, schema, options)?)
        }
        SetExpr::Insert(Statement::Insert(ins)) => {
            SetExpr::Insert(Statement::Insert(D::translate_insert(ins, schema, options)?))
        }
        SetExpr::Update(Statement::Update(upd)) => {
            SetExpr::Update(Statement::Update(translate_update::<D>(upd, schema, options)?))
        }
        SetExpr::Delete(Statement::Delete(del)) => {
            SetExpr::Delete(Statement::Delete(D::translate_delete(del, schema, options)?))
        }
        SetExpr::Table(_) | SetExpr::Merge(_) => {
            if D::IS_FORWARD {
                return Err(Error::UnsupportedSQLiteFeature(
                    "TABLE and MERGE expressions are not supported in SQLite".to_string(),
                ));
            }
            set_expr.clone()
        }
        // Catch-all for non-matching DML wrappers
        SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) => set_expr.clone(),
    })
}

/// Shared base `Query` translation used by both forward and reverse paths.
///
/// Forward-specific: strips locks and for_clause. Reverse preserves them.
/// Note: Forward path may call DISTINCT ON / GROUPING SETS rewrites separately
/// before falling through to this function.
pub(crate) fn translate_query_shared<D: TranslationDirection>(
    query: &Query,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Query, Error> {
    let order_by = translate_order_by_clause::<D>(query.order_by.as_ref(), schema, options)?;
    let settings = translate_query_settings::<D>(query.settings.as_ref(), schema, options)?;
    let pipe_operators = translate_pipe_operators::<D>(&query.pipe_operators, schema, options)?;
    let with = translate_with_clause::<D>(query.with.as_ref(), schema, options)?;
    let limit_clause = translate_limit_clause::<D>(query.limit_clause.as_ref(), schema, options)?;
    let fetch = translate_fetch_clause::<D>(query.fetch.as_ref(), schema, options)?;

    Ok(Query {
        with,
        body: Box::new(translate_set_expr_shared::<D>(&query.body, schema, options)?),
        order_by,
        limit_clause,
        fetch,
        // Forward: strip row-level locks (SQLite has no FOR UPDATE/SHARE).
        // Reverse: preserve as-is.
        locks: if D::IS_FORWARD { vec![] } else { query.locks.clone() },
        for_clause: if D::IS_FORWARD { None } else { query.for_clause.clone() },
        settings,
        format_clause: query.format_clause.clone(),
        pipe_operators,
    })
}

pub(crate) fn translate_pipe_operators<D: TranslationDirection>(
    pipe_operators: &[PipeOperator],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<PipeOperator>, Error> {
    pipe_operators
        .iter()
        .map(|pipe_operator| translate_pipe_operator::<D>(pipe_operator, schema, options))
        .collect::<Result<Vec<_>, _>>()
}

fn translate_measure<D: TranslationDirection>(
    measure: &Measure,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Measure, Error> {
    Ok(Measure {
        expr: D::translate_expr(&measure.expr, schema, options)?,
        alias: measure.alias.clone(),
    })
}

fn translate_symbol_definition<D: TranslationDirection>(
    symbol: &SymbolDefinition,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<SymbolDefinition, Error> {
    Ok(SymbolDefinition {
        symbol: symbol.symbol.clone(),
        definition: D::translate_expr(&symbol.definition, schema, options)?,
    })
}

#[allow(clippy::only_used_in_recursion)]
fn translate_json_table_column<D: TranslationDirection>(
    column: &sqlparser::ast::JsonTableColumn,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::JsonTableColumn, Error> {
    Ok(match column {
        sqlparser::ast::JsonTableColumn::Named(named) => {
            sqlparser::ast::JsonTableColumn::Named(named.clone())
        }
        sqlparser::ast::JsonTableColumn::ForOrdinality(ident) => {
            sqlparser::ast::JsonTableColumn::ForOrdinality(ident.clone())
        }
        sqlparser::ast::JsonTableColumn::Nested(nested) => {
            sqlparser::ast::JsonTableColumn::Nested(sqlparser::ast::JsonTableNestedColumn {
                path: nested.path.clone(),
                columns: nested
                    .columns
                    .iter()
                    .map(|column| translate_json_table_column::<D>(column, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            })
        }
    })
}

fn translate_xml_passing_argument<D: TranslationDirection>(
    argument: &XmlPassingArgument,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<XmlPassingArgument, Error> {
    Ok(XmlPassingArgument {
        expr: D::translate_expr(&argument.expr, schema, options)?,
        alias: argument.alias.clone(),
        by_value: argument.by_value,
    })
}

fn translate_xml_passing_clause<D: TranslationDirection>(
    passing: &XmlPassingClause,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<XmlPassingClause, Error> {
    Ok(XmlPassingClause {
        arguments: passing
            .arguments
            .iter()
            .map(|argument| translate_xml_passing_argument::<D>(argument, schema, options))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn translate_xml_table_column_option<D: TranslationDirection>(
    option: &XmlTableColumnOption,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<XmlTableColumnOption, Error> {
    Ok(match option {
        XmlTableColumnOption::NamedInfo { r#type, path, default, nullable } => {
            XmlTableColumnOption::NamedInfo {
                r#type: r#type.clone(),
                path: path
                    .as_ref()
                    .map(|expr| D::translate_expr(expr, schema, options))
                    .transpose()?,
                default: default
                    .as_ref()
                    .map(|expr| D::translate_expr(expr, schema, options))
                    .transpose()?,
                nullable: *nullable,
            }
        }
        XmlTableColumnOption::ForOrdinality => XmlTableColumnOption::ForOrdinality,
    })
}

fn translate_xml_table_column<D: TranslationDirection>(
    column: &XmlTableColumn,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<XmlTableColumn, Error> {
    Ok(XmlTableColumn {
        name: column.name.clone(),
        option: translate_xml_table_column_option::<D>(&column.option, schema, options)?,
    })
}

fn translate_xml_namespace_definition<D: TranslationDirection>(
    namespace: &XmlNamespaceDefinition,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<XmlNamespaceDefinition, Error> {
    Ok(XmlNamespaceDefinition {
        uri: D::translate_expr(&namespace.uri, schema, options)?,
        name: namespace.name.clone(),
    })
}

pub(crate) fn translate_table_with_joins<D: TranslationDirection>(
    table_with_joins: &TableWithJoins,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableWithJoins, Error> {
    let mut translated_joins = Vec::with_capacity(table_with_joins.joins.len());
    for join in &table_with_joins.joins {
        translated_joins.push(translate_join::<D>(join, schema, options)?);
    }

    Ok(TableWithJoins {
        relation: translate_table_factor::<D>(&table_with_joins.relation, schema, options)?,
        joins: translated_joins,
    })
}

pub(crate) fn translate_join<D: TranslationDirection>(
    join: &Join,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Join, Error> {
    Ok(Join {
        relation: translate_table_factor::<D>(&join.relation, schema, options)?,
        global: join.global,
        join_operator: translate_join_operator::<D>(&join.join_operator, schema, options)?,
    })
}

pub(crate) fn translate_join_operator<D: TranslationDirection>(
    join_operator: &JoinOperator,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<JoinOperator, Error> {
    Ok(match join_operator {
        JoinOperator::Join(constraint) => {
            JoinOperator::Join(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::Inner(constraint) => {
            JoinOperator::Inner(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::Left(constraint) => {
            JoinOperator::Left(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::LeftOuter(constraint) => {
            JoinOperator::LeftOuter(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::Right(constraint) => {
            JoinOperator::Right(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::RightOuter(constraint) => {
            JoinOperator::RightOuter(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::FullOuter(constraint) => {
            JoinOperator::FullOuter(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::CrossJoin(constraint) => {
            JoinOperator::CrossJoin(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::Semi(constraint) => {
            JoinOperator::Semi(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::LeftSemi(constraint) => {
            JoinOperator::LeftSemi(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::RightSemi(constraint) => {
            JoinOperator::RightSemi(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::Anti(constraint) => {
            JoinOperator::Anti(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::LeftAnti(constraint) => {
            JoinOperator::LeftAnti(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::RightAnti(constraint) => {
            JoinOperator::RightAnti(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::AsOf { constraint, match_condition } => {
            JoinOperator::AsOf {
                constraint: translate_join_constraint::<D>(constraint, schema, options)?,
                match_condition: D::translate_expr(match_condition, schema, options)?,
            }
        }
        JoinOperator::StraightJoin(constraint) => {
            JoinOperator::StraightJoin(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        // These operators don't have constraints that need translation
        JoinOperator::CrossApply | JoinOperator::OuterApply => join_operator.clone(),
    })
}

pub(crate) fn translate_join_constraint<D: TranslationDirection>(
    constraint: &JoinConstraint,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<JoinConstraint, Error> {
    Ok(match constraint {
        JoinConstraint::On(expr) => JoinConstraint::On(D::translate_expr(expr, schema, options)?),
        // USING and other constraints don't need expression translation
        JoinConstraint::Using(idents) => JoinConstraint::Using(idents.clone()),
        JoinConstraint::Natural => JoinConstraint::Natural,
        JoinConstraint::None => JoinConstraint::None,
    })
}

#[allow(clippy::too_many_lines)]
pub(crate) fn translate_table_factor<D: TranslationDirection>(
    table_factor: &TableFactor,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableFactor, Error> {
    Ok(match table_factor {
        TableFactor::Table {
            name,
            alias,
            args,
            with_hints,
            version,
            with_ordinality,
            partitions,
            json_path,
            sample,
            index_hints,
        } => {
            // Detect PostgreSQL table-valued functions called with arguments.
            // These appear as TableFactor::Table with args: Some(...).
            if D::IS_FORWARD && args.is_some() && is_generate_series_object_name(name) {
                return Err(generate_series_not_supported_error());
            }
            TableFactor::Table {
                name: D::translate_object_name(name, schema, options)?,
                alias: alias.clone(),
                args: args
                    .as_ref()
                    .map(|args| translate_table_function_args::<D>(args, schema, options))
                    .transpose()?,
                with_hints: with_hints
                    .iter()
                    .map(|hint| D::translate_expr(hint, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                version: version
                    .as_ref()
                    .map(|version| translate_table_version::<D>(version, schema, options))
                    .transpose()?,
                with_ordinality: *with_ordinality,
                partitions: partitions.clone(),
                json_path: json_path.clone(),
                sample: sample
                    .as_ref()
                    .map(|sample| translate_table_sample_kind::<D>(sample, schema, options))
                    .transpose()?,
                index_hints: index_hints.clone(),
            }
        }
        TableFactor::Derived { subquery, lateral, alias, sample } => {
            TableFactor::Derived {
                subquery: Box::new(D::translate_query(subquery, schema, options)?),
                lateral: *lateral,
                alias: alias.clone(),
                sample: sample
                    .as_ref()
                    .map(|sample| translate_table_sample_kind::<D>(sample, schema, options))
                    .transpose()?,
            }
        }
        TableFactor::TableFunction { expr, alias } => {
            TableFactor::TableFunction {
                expr: D::translate_expr(expr, schema, options)?,
                alias: alias.clone(),
            }
        }
        TableFactor::Function { lateral, name, args, alias } => {
            // Detect PostgreSQL table-valued functions that have no SQLite equivalent
            if D::IS_FORWARD && is_generate_series_object_name(name) {
                return Err(generate_series_not_supported_error());
            }
            TableFactor::Function {
                lateral: *lateral,
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| translate_function_arg::<D>(arg, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                alias: alias.clone(),
            }
        }
        TableFactor::UNNEST {
            alias,
            array_exprs,
            with_offset,
            with_offset_alias,
            with_ordinality,
        } => {
            TableFactor::UNNEST {
                alias: alias.clone(),
                array_exprs: array_exprs
                    .iter()
                    .map(|expr| D::translate_expr(expr, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                with_offset: *with_offset,
                with_offset_alias: with_offset_alias.clone(),
                with_ordinality: *with_ordinality,
            }
        }
        TableFactor::JsonTable { json_expr, json_path, columns, alias } => {
            TableFactor::JsonTable {
                json_expr: D::translate_expr(json_expr, schema, options)?,
                json_path: json_path.clone(),
                columns: columns
                    .iter()
                    .map(|column| translate_json_table_column::<D>(column, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                alias: alias.clone(),
            }
        }
        TableFactor::OpenJsonTable { json_expr, json_path, columns, alias } => {
            TableFactor::OpenJsonTable {
                json_expr: D::translate_expr(json_expr, schema, options)?,
                json_path: json_path.clone(),
                columns: columns.clone(),
                alias: alias.clone(),
            }
        }
        TableFactor::NestedJoin { table_with_joins, alias } => {
            TableFactor::NestedJoin {
                table_with_joins: Box::new(translate_table_with_joins::<D>(
                    table_with_joins,
                    schema,
                    options,
                )?),
                alias: alias.clone(),
            }
        }
        TableFactor::Pivot {
            table,
            aggregate_functions,
            value_column,
            value_source,
            default_on_null,
            alias,
        } => {
            TableFactor::Pivot {
                table: Box::new(translate_table_factor::<D>(table, schema, options)?),
                aggregate_functions: aggregate_functions
                    .iter()
                    .map(|expr_with_alias| {
                        translate_expr_with_alias::<D>(expr_with_alias, schema, options)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                value_column: value_column
                    .iter()
                    .map(|expr| D::translate_expr(expr, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                value_source: translate_pivot_value_source::<D>(value_source, schema, options)?,
                default_on_null: default_on_null
                    .as_ref()
                    .map(|expr| D::translate_expr(expr, schema, options))
                    .transpose()?,
                alias: alias.clone(),
            }
        }
        TableFactor::Unpivot { table, value, name, columns, null_inclusion, alias } => {
            TableFactor::Unpivot {
                table: Box::new(translate_table_factor::<D>(table, schema, options)?),
                value: D::translate_expr(value, schema, options)?,
                name: name.clone(),
                columns: columns
                    .iter()
                    .map(|expr_with_alias| {
                        translate_expr_with_alias::<D>(expr_with_alias, schema, options)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                null_inclusion: null_inclusion.clone(),
                alias: alias.clone(),
            }
        }
        TableFactor::MatchRecognize {
            table,
            partition_by,
            order_by,
            measures,
            rows_per_match,
            after_match_skip,
            pattern,
            symbols,
            alias,
        } => {
            TableFactor::MatchRecognize {
                table: Box::new(translate_table_factor::<D>(table, schema, options)?),
                partition_by: partition_by
                    .iter()
                    .map(|expr| D::translate_expr(expr, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                order_by: order_by
                    .iter()
                    .map(|order_by_expr| {
                        translate_order_by_expr::<D>(order_by_expr, schema, options)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                measures: measures
                    .iter()
                    .map(|measure| translate_measure::<D>(measure, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                rows_per_match: rows_per_match.clone(),
                after_match_skip: after_match_skip.clone(),
                pattern: pattern.clone(),
                symbols: symbols
                    .iter()
                    .map(|symbol| translate_symbol_definition::<D>(symbol, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                alias: alias.clone(),
            }
        }
        TableFactor::XmlTable { namespaces, row_expression, passing, columns, alias } => {
            TableFactor::XmlTable {
                namespaces: namespaces
                    .iter()
                    .map(|namespace| {
                        translate_xml_namespace_definition::<D>(namespace, schema, options)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                row_expression: D::translate_expr(row_expression, schema, options)?,
                passing: translate_xml_passing_clause::<D>(passing, schema, options)?,
                columns: columns
                    .iter()
                    .map(|column| translate_xml_table_column::<D>(column, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                alias: alias.clone(),
            }
        }
        TableFactor::SemanticView { name, dimensions, metrics, facts, where_clause, alias } => {
            TableFactor::SemanticView {
                name: name.clone(),
                dimensions: dimensions
                    .iter()
                    .map(|expr| D::translate_expr(expr, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                metrics: metrics
                    .iter()
                    .map(|expr| D::translate_expr(expr, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                facts: facts
                    .iter()
                    .map(|expr| D::translate_expr(expr, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                where_clause: where_clause
                    .as_ref()
                    .map(|expr| D::translate_expr(expr, schema, options))
                    .transpose()?,
                alias: alias.clone(),
            }
        }
    })
}

pub(crate) fn translate_select_item<D: TranslationDirection>(
    item: &SelectItem,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<SelectItem, Error> {
    Ok(match item {
        SelectItem::UnnamedExpr(expr) => {
            SelectItem::UnnamedExpr(D::translate_expr(expr, schema, options)?)
        }
        SelectItem::ExprWithAlias { expr, alias } => {
            SelectItem::ExprWithAlias {
                expr: D::translate_expr(expr, schema, options)?,
                alias: alias.clone(),
            }
        }
        // Wildcards and qualified wildcards don't need translation
        other => other.clone(),
    })
}

pub(crate) fn translate_returning<D: TranslationDirection>(
    returning: Option<&Vec<SelectItem>>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Vec<SelectItem>>, Error> {
    match returning {
        Some(items) => {
            let mut translated = Vec::with_capacity(items.len());
            for item in items {
                translated.push(translate_select_item::<D>(item, schema, options)?);
            }
            Ok(Some(translated))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{
            Expr, JoinConstraint, JoinOperator, Query, SelectItem, SetExpr, Statement, TableFactor,
            ValueWithSpan,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        TranslationDirection, translate_join, translate_join_constraint, translate_join_operator,
        translate_returning, translate_select_item, translate_table_factor,
        translate_table_with_joins,
    };
    use crate::{errors::Error, prelude::Pg2SqliteOptions};

    struct IdentityDirection;

    impl TranslationDirection for IdentityDirection {
        fn translate_expr(
            expr: &Expr,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
        ) -> Result<Expr, Error> {
            Ok(expr.clone())
        }

        fn translate_query(
            query: &Query,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
        ) -> Result<Query, Error> {
            Ok(query.clone())
        }

        fn translate_insert(
            insert: &sqlparser::ast::Insert,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
        ) -> Result<sqlparser::ast::Insert, Error> {
            Ok(insert.clone())
        }

        fn translate_delete(
            delete: &sqlparser::ast::Delete,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
        ) -> Result<sqlparser::ast::Delete, Error> {
            Ok(delete.clone())
        }
    }

    struct NestingDirection;

    impl TranslationDirection for NestingDirection {
        fn translate_expr(
            expr: &Expr,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
        ) -> Result<Expr, Error> {
            Ok(Expr::Nested(Box::new(expr.clone())))
        }

        fn translate_query(
            query: &Query,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
        ) -> Result<Query, Error> {
            Ok(query.clone())
        }

        fn translate_insert(
            insert: &sqlparser::ast::Insert,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
        ) -> Result<sqlparser::ast::Insert, Error> {
            Ok(insert.clone())
        }

        fn translate_delete(
            delete: &sqlparser::ast::Delete,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
        ) -> Result<sqlparser::ast::Delete, Error> {
            Ok(delete.clone())
        }
    }

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap()
    }

    fn parse_expr(sql: &str) -> Expr {
        Parser::new(&PostgreSqlDialect {}).try_with_sql(sql).unwrap().parse_expr().unwrap()
    }

    fn parse_query(sql: &str) -> Query {
        let stmts = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap();
        match stmts.into_iter().next().unwrap() {
            Statement::Query(query) => *query,
            other => panic!("expected query statement, got: {other:?}"),
        }
    }

    #[test]
    fn translates_join_structures_and_select_items() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let query = parse_query(
            "SELECT t.a AS a1 FROM t INNER JOIN u ON t.id = u.id LEFT JOIN v ON u.id = v.uid",
        );
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected select");
        };

        let translated = translate_table_with_joins::<IdentityDirection>(
            select.from.first().unwrap(),
            &schema,
            &options,
        )
        .unwrap();
        assert_eq!(translated.joins.len(), 2);

        let unnamed = SelectItem::UnnamedExpr(Expr::Identifier(sqlparser::ast::Ident::new("a")));
        let named = SelectItem::ExprWithAlias {
            expr: Expr::Identifier(sqlparser::ast::Ident::new("b")),
            alias: sqlparser::ast::Ident::new("b1"),
        };
        assert!(matches!(
            translate_select_item::<IdentityDirection>(&unnamed, &schema, &options).unwrap(),
            SelectItem::UnnamedExpr(_)
        ));
        assert!(matches!(
            translate_select_item::<IdentityDirection>(&named, &schema, &options).unwrap(),
            SelectItem::ExprWithAlias { .. }
        ));
    }

    #[test]
    fn translates_all_join_operator_variants() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let on = JoinConstraint::On(Expr::Value(ValueWithSpan::from(
            sqlparser::ast::Value::Boolean(true),
        )));

        let operators = vec![
            JoinOperator::Join(on.clone()),
            JoinOperator::Inner(on.clone()),
            JoinOperator::Left(on.clone()),
            JoinOperator::LeftOuter(on.clone()),
            JoinOperator::Right(on.clone()),
            JoinOperator::RightOuter(on.clone()),
            JoinOperator::FullOuter(on.clone()),
            JoinOperator::CrossJoin(on.clone()),
            JoinOperator::Semi(on.clone()),
            JoinOperator::LeftSemi(on.clone()),
            JoinOperator::RightSemi(on.clone()),
            JoinOperator::Anti(on.clone()),
            JoinOperator::LeftAnti(on.clone()),
            JoinOperator::RightAnti(on.clone()),
            JoinOperator::AsOf {
                constraint: on.clone(),
                match_condition: Expr::Value(ValueWithSpan::from(sqlparser::ast::Value::Number(
                    "1".to_string(),
                    false,
                ))),
            },
            JoinOperator::StraightJoin(on.clone()),
            JoinOperator::CrossApply,
            JoinOperator::OuterApply,
        ];

        for op in &operators {
            let _ = translate_join_operator::<IdentityDirection>(op, &schema, &options).unwrap();
        }

        let _ = translate_join_constraint::<IdentityDirection>(&on, &schema, &options).unwrap();
    }

    #[test]
    fn translates_table_factor_and_returning() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let query = parse_query("SELECT * FROM (SELECT 1) AS q");
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected select");
        };

        let derived = &select.from[0].relation;
        let _ = translate_table_factor::<IdentityDirection>(derived, &schema, &options).unwrap();

        let nested_query = parse_query("SELECT * FROM (t JOIN u ON t.id = u.id) AS z");
        let sqlparser::ast::SetExpr::Select(nested_select) = nested_query.body.as_ref() else {
            panic!("expected select");
        };
        let nested_factor = &nested_select.from[0].relation;
        if let TableFactor::NestedJoin { .. } = nested_factor {
            let _ = translate_table_factor::<IdentityDirection>(nested_factor, &schema, &options)
                .unwrap();
        }

        let joined_query = parse_query("SELECT * FROM t INNER JOIN u ON t.id = u.id");
        let SetExpr::Select(joined_select) = joined_query.body.as_ref() else {
            panic!("expected select");
        };
        let manual_nested = TableFactor::NestedJoin {
            table_with_joins: Box::new(joined_select.from[0].clone()),
            alias: None,
        };
        let translated_manual =
            translate_table_factor::<IdentityDirection>(&manual_nested, &schema, &options).unwrap();
        assert!(matches!(translated_manual, TableFactor::NestedJoin { .. }));

        let returning_items = vec![
            SelectItem::UnnamedExpr(Expr::Identifier(sqlparser::ast::Ident::new("id"))),
            SelectItem::ExprWithAlias {
                expr: Expr::Identifier(sqlparser::ast::Ident::new("name")),
                alias: sqlparser::ast::Ident::new("n"),
            },
        ];
        assert_eq!(
            translate_returning::<IdentityDirection>(Some(&returning_items), &schema, &options)
                .unwrap()
                .unwrap()
                .len(),
            2
        );
        assert!(
            translate_returning::<IdentityDirection>(None, &schema, &options).unwrap().is_none()
        );
    }

    #[test]
    fn translate_join_preserves_global_flag() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let query = parse_query("SELECT * FROM t INNER JOIN u ON t.id = u.id");
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected select");
        };
        let mut join = select.from[0].joins[0].clone();
        join.global = true;
        let translated = translate_join::<IdentityDirection>(&join, &schema, &options).unwrap();
        assert!(translated.global);
    }

    #[test]
    fn asof_and_expr_alias_apply_expr_translation_direction() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let as_of = JoinOperator::AsOf {
            constraint: JoinConstraint::On(parse_expr("t.id = u.id")),
            match_condition: parse_expr("t.id > u.id"),
        };
        let translated_as_of =
            translate_join_operator::<NestingDirection>(&as_of, &schema, &options).unwrap();
        let JoinOperator::AsOf { match_condition, .. } = translated_as_of else {
            panic!("expected AS OF join");
        };
        assert!(matches!(match_condition, Expr::Nested(_)));

        let alias_item = SelectItem::ExprWithAlias {
            expr: parse_expr("a"),
            alias: sqlparser::ast::Ident::new("a1"),
        };
        let translated_alias =
            translate_select_item::<NestingDirection>(&alias_item, &schema, &options).unwrap();
        let SelectItem::ExprWithAlias { expr, .. } = translated_alias else {
            panic!("expected alias expression");
        };
        assert!(matches!(expr, Expr::Nested(_)));
    }
}
