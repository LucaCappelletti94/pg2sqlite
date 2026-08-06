//! Shared helper functions for translating table references, joins, and select
//! items. Generic over translation direction (forward or reverse).

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
use core::ops::ControlFlow;

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    Assignment, AssignmentTarget, BinaryOperator, ConnectByKind, DataType, Expr, ExprWithAlias,
    ExprWithAliasAndOrderBy, Fetch, FromTable, FunctionArg, FunctionArgExpr,
    FunctionArgumentClause, FunctionArgumentList, FunctionArguments, GroupByExpr, HavingBound,
    Join, JoinConstraint, JoinOperator, LateralView, LimitClause, ListAggOnOverflow, Measure,
    NamedWindowDefinition, NamedWindowExpr, ObjectName, ObjectNamePart, OrderBy, OrderByExpr,
    OrderByKind, PipeOperator, PivotValueSource, Query, SelectItem, SetExpr, Setting, Statement,
    SymbolDefinition, TableFactor, TableFunctionArgs, TableSample, TableSampleBucket,
    TableSampleKind, TableSampleQuantity, TableVersion, TableWithJoins, UnaryOperator,
    UpdateTableFromKind, Value, ValueWithSpan, Values, WindowFrame, WindowFrameBound, WindowSpec,
    WindowType, With, WithFill, XmlNamespaceDefinition, XmlPassingArgument, XmlPassingClause,
    XmlTableColumn, XmlTableColumnOption, visit_expressions,
};

use crate::{
    errors::Error,
    impls::{
        object_name::{last_ident, table_with_implicit_public_lookup},
        translator_impls::{
            uuid::{
                is_blob_uuid_representation, maybe_wrap_text_uuid_literal, uuid_columns_of_table,
            },
            vector::{maybe_wrap_text_vector_literal, vector_columns_of_table},
        },
    },
    prelude::Pg2SqliteOptions,
};

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

/// Returns the standardised error for `WITH ORDINALITY`, which SQLite has no
/// clause for.
///
/// Refused rather than dropped, because the ordinality column is projected by
/// the query around it, so losing the clause loses a column the caller selects.
/// `UNNEST ... WITH ORDINALITY` does NOT come here: forward translation lowers
/// it onto `json_each`, whose `key` column supplies the ordinality.
#[must_use]
pub(crate) fn with_ordinality_not_supported_error() -> Error {
    Error::UnsupportedSQLiteFeature(
        "WITH ORDINALITY is not supported in SQLite, which has no clause that numbers the rows of \
         a FROM item. Number them in the query instead, with ROW_NUMBER() OVER (), or use UNNEST, \
         which is translated through json_each and does supply an ordinality column."
            .to_string(),
    )
}

/// Returns the standardised error for `NULLS NOT DISTINCT`.
///
/// PostgreSQL makes two NULL rows collide under it, and SQLite's unique
/// indexes always treat NULLs as distinct, with no clause to change that.
/// Verified on both: PostgreSQL 16 answers `duplicate key value violates
/// unique constraint` for a second NULL, SQLite accepts it. So the clause
/// cannot be dropped, which would let through rows PostgreSQL refuses, and it
/// cannot be emitted either, which is `near "NULLS": syntax error`.
///
/// `NULLS DISTINCT`, PostgreSQL's default, IS what SQLite does, so that
/// spelling is dropped rather than refused.
#[must_use]
pub(crate) fn nulls_not_distinct_not_supported_error() -> Error {
    Error::UnsupportedSQLiteFeature(
        "NULLS NOT DISTINCT is not supported in SQLite, whose unique indexes always treat NULLs as \
         distinct, so the constraint would accept rows PostgreSQL rejects. Add a CHECK that the \
         column is NOT NULL, or enforce the rule with a trigger."
            .to_string(),
    )
}

/// The name of the column `expr` refers to.
///
/// The qualifier of a compound name is dropped, since it may be an alias rather
/// than a table.
fn referenced_column_name(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Identifier(ident) => Some(ident.value.as_str()),
        Expr::CompoundIdentifier(parts) => Some(parts.last()?.value.as_str()),
        Expr::Nested(inner) => referenced_column_name(inner),
        _ => None,
    }
}

/// Fold over every column in the schema named by `expr`, stopping as soon as
/// `read` returns `None`.
///
/// The answer has to be unanimous, since the qualifier is dropped, and the
/// early stop is what keeps this off the hot path: a first column that already
/// settles the question ends the walk instead of visiting every table.
///
/// `read` needs the structured type, so the parsed DDL is read directly rather
/// than through `ColumnLike::data_type`, which answers a normalised token.
fn unanimous_declared<T: PartialEq>(
    expr: &Expr,
    schema: &ParserDB,
    read: impl Fn(&DataType) -> Option<T>,
) -> Option<T> {
    let column_name = referenced_column_name(expr)?;
    let mut answer: Option<T> = None;
    for table in schema.tables() {
        let Ok(mut columns) = table.columns(schema) else { continue };
        let Some(column) = columns.find(|c| c.column_name().eq_ignore_ascii_case(column_name))
        else {
            continue;
        };
        let read = read(&column.attribute().data_type)?;
        match &answer {
            Some(previous) if *previous != read => return None,
            Some(_) => {}
            None => answer = Some(read),
        }
    }
    answer
}

/// True when the column `expr` names is declared with a type `predicate`
/// accepts.
pub(crate) fn every_declared_type_matches(
    expr: &Expr,
    schema: &ParserDB,
    predicate: impl Fn(&str) -> bool,
) -> bool {
    unanimous_declared(expr, schema, |data_type| predicate(&data_type.to_string()).then_some(()))
        .is_some()
}

/// The scale of `expr` when it is a `NUMERIC` value held as minor units.
pub(crate) fn numeric_scale(expr: &Expr, schema: &ParserDB) -> Option<u32> {
    numeric_precision_and_scale_of(expr, schema).map(|(_, scale)| scale)
}

/// The declared precision of `expr`, which D1's multiplication rule needs.
pub(crate) fn declared_numeric_precision(expr: &Expr, schema: &ParserDB) -> Option<u64> {
    numeric_precision_and_scale_of(expr, schema).map(|(precision, _)| precision)
}

fn numeric_precision_and_scale_of(expr: &Expr, schema: &ParserDB) -> Option<(u64, u32)> {
    let read = |data_type: &DataType| {
        let (DataType::Numeric(info) | DataType::Decimal(info)) = data_type else { return None };
        crate::impls::translator_impls::data_type::numeric_precision_and_scale(info).ok()
    };
    match expr {
        Expr::Nested(inner) => numeric_precision_and_scale_of(inner, schema),
        Expr::Cast { data_type, .. } => read(data_type),
        _ => unanimous_declared(expr, schema, read),
    }
}

/// Move a value held as minor units from `from` scale to `to` scale.
///
/// Growing the scale multiplies. Shrinking it divides, and PostgreSQL rounds
/// half away from zero where SQLite's integer division truncates toward it, so
/// half a unit is added with the value's sign first. `1.005::numeric(10,2)` is
/// 1.01 and `(-1.005)::numeric(10,2)` is -1.01.
pub(crate) fn rescale_minor_units(value: Expr, from: u32, to: u32) -> Expr {
    if from == to {
        return value;
    }
    if to > from {
        return Expr::Nested(Box::new(Expr::BinaryOp {
            left: Box::new(value),
            op: BinaryOperator::Multiply,
            right: Box::new(crate::impls::function_helpers::number_literal(
                &10_i128.pow(to - from).to_string(),
            )),
        }));
    }

    let divisor = 10_i128.pow(from - to);
    let half = divisor / 2;
    // `value + half * sign(value)` before the truncating division turns
    // truncation toward zero into rounding away from it.
    let biased = Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(value.clone()),
        op: BinaryOperator::Plus,
        right: Box::new(Expr::BinaryOp {
            left: Box::new(crate::impls::function_helpers::number_literal(&half.to_string())),
            op: BinaryOperator::Multiply,
            right: Box::new(crate::impls::function_helpers::simple_function_expr(
                "sign",
                vec![value],
                None,
            )),
        }),
    }));
    Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(biased),
        op: BinaryOperator::Divide,
        right: Box::new(crate::impls::function_helpers::number_literal(&divisor.to_string())),
    }))
}

/// Rewrite a decimal literal as the integer count of minor units at `scale`.
///
/// The digits are moved rather than multiplied as a float, so `19.99` at scale
/// 2 is 1999 and not 1998.9999999999998. A literal finer than the column is
/// refused rather than rounded.
pub(crate) fn scale_decimal_literal(expr: &Expr, scale: u32) -> Result<Option<Expr>, Error> {
    let (negated, digits) = match expr {
        Expr::Value(ValueWithSpan { value: Value::Number(digits, _), .. }) => (false, digits),
        Expr::UnaryOp { op: UnaryOperator::Minus, expr } => {
            match expr.as_ref() {
                Expr::Value(ValueWithSpan { value: Value::Number(digits, _), .. }) => {
                    (true, digits)
                }
                _ => return Ok(None),
            }
        }
        _ => return Ok(None),
    };

    if digits.contains(['e', 'E']) {
        return Err(Error::UnsupportedSQLiteFeature(format!(
            "the literal {digits} is in exponent notation, which this translator does not scale \
             onto a NUMERIC column. Write it in full."
        )));
    }

    let (whole, fraction) = digits.split_once('.').unwrap_or((digits.as_str(), ""));
    let fraction_digits = u32::try_from(fraction.len()).unwrap_or(u32::MAX);
    if fraction_digits > scale {
        return Err(Error::UnsupportedSQLiteFeature(format!(
            "the literal {digits} has {fraction_digits} decimal places and the column holds \
             {scale}. PostgreSQL would round it, which silently changes the value, so write it \
             at the column's scale instead."
        )));
    }

    // Provably in range: `fraction_digits <= scale` was just checked, and a
    // scale is at most MAX_NUMERIC_PRECISION.
    let padding = "0".repeat(usize::try_from(scale - fraction_digits).unwrap_or(0));
    let minor_units = format!("{}{whole}{fraction}{padding}", if negated { "-" } else { "" });
    Ok(Some(Expr::Value(ValueWithSpan {
        value: Value::Number(minor_units, false),
        span: sqlparser::tokenizer::Span::empty(),
    })))
}

/// True when `expr` is the bare `DEFAULT` keyword.
///
/// `sqlparser` has no `Expr` variant for it: `DEFAULT` is not reserved in
/// expression position, so it arrives as a plain identifier, which is why this
/// is a name comparison rather than a pattern match. A column genuinely called
/// `default` has to be quoted to be referenced, and a quoted identifier is not
/// matched here.
#[must_use]
pub(crate) fn is_default_keyword(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Identifier(ident)
            if ident.quote_style.is_none() && ident.value.eq_ignore_ascii_case("default")
    )
}

/// Returns the error for a `DEFAULT` outside an `INSERT`.
///
/// Inside one it is substituted for the column's declared default, which is
/// what `insert.rs` does before the source is translated, so anything reaching
/// here is in a context where PostgreSQL does not allow the keyword either: it
/// answers "DEFAULT is not allowed in this context".
#[must_use]
pub(crate) fn default_outside_an_insert_error() -> Error {
    Error::UnsupportedSQLiteFeature(
        "DEFAULT is only meaningful in a VALUES row of an INSERT, where it stands for the \
         column's declared default. PostgreSQL rejects it anywhere else, and SQLite has no form \
         of it at all."
            .to_string(),
    )
}

/// Extracts a stable-ish variant name from debug output.
#[must_use]
pub(crate) fn debug_variant_name(value: &impl core::fmt::Debug) -> String {
    let debug = format!("{value:?}");
    debug.split(['(', '{', ' ']).next().unwrap_or("Unknown").to_string()
}

/// Translates `expr` using direction `D`, delegating structural recursion to
/// [`crate::impls::expr_helpers::try_map_expr_children`]. Callers should handle
/// direction-specific semantic transforms before falling through.
pub(crate) fn translate_expr_recursive<D: TranslationDirection>(
    expr: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    crate::impls::expr_helpers::try_map_expr_children(
        expr,
        &|e| D::translate_expr(e, schema, options),
        &|q| D::translate_query(q, schema, options),
    )
}

/// Translates DO UPDATE assignments and WHERE inside an ON CONFLICT clause.
pub(crate) fn translate_on_conflict_do_update<D: TranslationDirection>(
    on_conflict: &sqlparser::ast::OnConflict,
    do_update: &sqlparser::ast::DoUpdate,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::OnInsert, Error> {
    let assignments = do_update
        .assignments
        .iter()
        .map(|a| {
            Ok(Assignment {
                target: a.target.clone(),
                value: D::translate_expr(&a.value, schema, options)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let selection = do_update
        .selection
        .as_ref()
        .map(|expr| D::translate_expr(expr, schema, options))
        .transpose()?;
    Ok(sqlparser::ast::OnInsert::OnConflict(sqlparser::ast::OnConflict {
        conflict_target: on_conflict.conflict_target.clone(),
        action: sqlparser::ast::OnConflictAction::DoUpdate(sqlparser::ast::DoUpdate {
            assignments,
            selection,
        }),
    }))
}

/// Translate the core fields shared by forward and reverse `Delete`
/// translation: `selection`, `from`, `returning`, `order_by`, and `limit`.
#[allow(clippy::type_complexity)]
pub(crate) fn translate_delete_core<D: TranslationDirection>(
    delete: &sqlparser::ast::Delete,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<(Option<Expr>, FromTable, Option<Vec<SelectItem>>, Vec<OrderByExpr>, Option<Expr>), Error>
{
    let selection =
        delete.selection.as_ref().map(|e| D::translate_expr(e, schema, options)).transpose()?;
    let from = map_from_table(&delete.from, |table| {
        translate_table_with_joins::<D>(table, schema, options)
    })?;
    let returning = translate_returning::<D>(delete.returning.as_ref(), schema, options)?;
    let order_by = delete
        .order_by
        .iter()
        .map(|expr| translate_order_by_expr::<D>(expr, schema, options))
        .collect::<Result<Vec<_>, _>>()?;
    let limit =
        delete.limit.as_ref().map(|expr| D::translate_expr(expr, schema, options)).transpose()?;
    Ok((selection, from, returning, order_by, limit))
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
        FunctionArgExpr::WildcardWithOptions(opts) => {
            FunctionArgExpr::WildcardWithOptions(opts.clone())
        }
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
        TableVersion::Changes { changes, at, end } => {
            TableVersion::Changes {
                changes: D::translate_expr(changes, schema, options)?,
                at: D::translate_expr(at, schema, options)?,
                end: end.as_ref().map(|e| D::translate_expr(e, schema, options)).transpose()?,
            }
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
    let options_out = if D::IS_FORWARD {
        // The two databases default oppositely, PostgreSQL ASC NULLS LAST and
        // DESC NULLS FIRST against SQLite's reverse, so an absent clause is
        // filled in rather than left out. An absent direction is ASC.
        let descending =
            matches!(order_by_expr.options.sort, Some(sqlparser::ast::OrderBySort::Desc));
        sqlparser::ast::OrderByOptions {
            sort: order_by_expr.options.sort.clone(),
            nulls_first: Some(order_by_expr.options.nulls_first.unwrap_or(descending)),
        }
    } else {
        order_by_expr.options.clone()
    };

    Ok(OrderByExpr {
        expr: D::translate_expr(&order_by_expr.expr, schema, options)?,
        options: options_out,
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
        order_by: expr_with_alias_and_order_by.order_by.clone(),
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
                    debug_variant_name(&translated_expr)
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
                OrderByKind::All(all) => OrderByKind::All(all.clone()),
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
                // PostgreSQL has no comma form. The spelling puts the offset
                // first, so `LIMIT 5, 10` is offset 5 and limit 10.
                LimitClause::OffsetCommaLimit { offset, limit } if !D::IS_FORWARD => {
                    LimitClause::LimitOffset {
                        limit: Some(D::translate_expr(limit, schema, options)?),
                        offset: Some(sqlparser::ast::Offset {
                            value: D::translate_expr(offset, schema, options)?,
                            rows: sqlparser::ast::OffsetRows::None,
                        }),
                        limit_by: Vec::new(),
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
            .map(|row| -> Result<sqlparser::ast::Parens<Vec<Expr>>, Error> {
                let translated = row
                    .content
                    .iter()
                    .map(|expr| {
                        if D::IS_FORWARD && is_default_keyword(expr) {
                            return Err(default_outside_an_insert_error());
                        }
                        D::translate_expr(expr, schema, options)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(sqlparser::ast::Parens {
                    opening_token: row.opening_token.clone(),
                    content: translated,
                    closing_token: row.closing_token.clone(),
                })
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

/// Shared UPDATE translation. Forward rejects joins on the target table.
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

    // Best-effort: falls back to passthrough for unknown tables (CTEs, etc.).
    // Both wraps are forward-only. Reverse receives an already-rewritten input.
    let (vector_cols, uuid_cols): (Vec<(String, bool)>, Vec<String>) = if D::IS_FORWARD {
        match &update.table.relation {
            TableFactor::Table { name, .. } => {
                table_with_implicit_public_lookup(schema, name)
                    .ok()
                    .flatten()
                    .map(|table| {
                        let v = vector_columns_of_table(table, schema).unwrap_or_default();
                        let u = if is_blob_uuid_representation(options) {
                            uuid_columns_of_table(table, schema).unwrap_or_default()
                        } else {
                            Vec::new()
                        };
                        (v, u)
                    })
                    .unwrap_or_default()
            }
            _ => (Vec::new(), Vec::new()),
        }
    } else {
        (Vec::new(), Vec::new())
    };

    let assignments = update
        .assignments
        .iter()
        .map(|a| {
            let translated_value = D::translate_expr(&a.value, schema, options)?;
            let column_name = match &a.target {
                AssignmentTarget::ColumnName(name) => last_ident(name).map(|i| i.value.clone()),
                AssignmentTarget::Tuple(_) => None,
            };
            let final_value = match column_name.as_deref() {
                Some(name) => {
                    if let Some(is_halfvec) = vector_cols
                        .iter()
                        .find(|(col, _)| col.eq_ignore_ascii_case(name))
                        .map(|(_, is_halfvec)| *is_halfvec)
                    {
                        maybe_wrap_text_vector_literal(translated_value, is_halfvec)
                    } else if uuid_cols.iter().any(|col| col.eq_ignore_ascii_case(name)) {
                        maybe_wrap_text_uuid_literal(translated_value, options)?
                    } else {
                        translated_value
                    }
                }
                None => translated_value,
            };
            Ok(Assignment { target: a.target.clone(), value: final_value })
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

    let translated = sqlparser::ast::Update {
        update_token: update.update_token.clone(),
        optimizer_hints: update.optimizer_hints.clone(),
        table: translate_table_with_joins::<D>(&update.table, schema, options)?,
        assignments,
        from,
        selection,
        returning,
        output: update.output.clone(),
        or: update.or,
        order_by: update.order_by.clone(),
        limit,
    };

    // Route ST_* WHERE predicates through the rtree shadow via IN-subquery.
    // Single-target-table only. UPDATE ... FROM and joined targets pass through.
    if D::IS_FORWARD
        && let Some(rewritten) =
            crate::impls::translator_impls::postgis::try_rewrite_spatial_update(
                &translated,
                options,
            )?
    {
        return Ok(rewritten);
    }
    Ok(translated)
}

/// Shared DISTINCT translation. Forward rejects `DISTINCT ON` because SQLite
/// does not support it. Reverse translates `DISTINCT ON` expressions.
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

/// Shared TOP translation. Forward clones as-is. Reverse translates quantity.
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

    let translated = sqlparser::ast::Select {
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
        optimizer_hints: select.optimizer_hints.clone(),
        select_modifiers: select.select_modifiers.clone(),
    };

    // Hooked here so DISTINCT ON and GROUPING SETS rewrites that call
    // translate_select_shared directly also receive spatial rewriting.
    if D::IS_FORWARD
        && let Some(rewritten) =
            crate::impls::translator_impls::postgis::try_rewrite_spatial_select(
                &translated,
                options,
            )?
    {
        return Ok(rewritten);
    }
    Ok(translated)
}

/// Shared `SetExpr` translation. Forward errors on `Table` and `Merge`.
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
        SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) => set_expr.clone(),
    })
}

/// Shared `Query` translation. Forward strips row locks and `for_clause`.
/// Callers may apply DISTINCT ON and GROUPING SETS rewrites first.
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

/// Map a [`JoinOperator`] by applying `f_constraint` to each constraint and
/// `f_expr` to the `AsOf::match_condition`.  Replaces 16+-arm match blocks
/// in `rls.rs`, `plpgsql/translator.rs`, and this module.
pub(crate) fn map_join_operator<E>(
    op: &JoinOperator,
    f_constraint: &impl Fn(&JoinConstraint) -> Result<JoinConstraint, E>,
    f_expr: &impl Fn(&Expr) -> Result<Expr, E>,
) -> Result<JoinOperator, E> {
    Ok(match op {
        JoinOperator::Join(c) => JoinOperator::Join(f_constraint(c)?),
        JoinOperator::Inner(c) => JoinOperator::Inner(f_constraint(c)?),
        JoinOperator::Left(c) => JoinOperator::Left(f_constraint(c)?),
        JoinOperator::LeftOuter(c) => JoinOperator::LeftOuter(f_constraint(c)?),
        JoinOperator::Right(c) => JoinOperator::Right(f_constraint(c)?),
        JoinOperator::RightOuter(c) => JoinOperator::RightOuter(f_constraint(c)?),
        JoinOperator::FullOuter(c) => JoinOperator::FullOuter(f_constraint(c)?),
        JoinOperator::CrossJoin(c) => JoinOperator::CrossJoin(f_constraint(c)?),
        JoinOperator::Semi(c) => JoinOperator::Semi(f_constraint(c)?),
        JoinOperator::LeftSemi(c) => JoinOperator::LeftSemi(f_constraint(c)?),
        JoinOperator::RightSemi(c) => JoinOperator::RightSemi(f_constraint(c)?),
        JoinOperator::Anti(c) => JoinOperator::Anti(f_constraint(c)?),
        JoinOperator::LeftAnti(c) => JoinOperator::LeftAnti(f_constraint(c)?),
        JoinOperator::RightAnti(c) => JoinOperator::RightAnti(f_constraint(c)?),
        JoinOperator::StraightJoin(c) => JoinOperator::StraightJoin(f_constraint(c)?),
        JoinOperator::AsOf { constraint, match_condition } => {
            JoinOperator::AsOf {
                constraint: f_constraint(constraint)?,
                match_condition: f_expr(match_condition)?,
            }
        }
        JoinOperator::CrossApply
        | JoinOperator::OuterApply
        | JoinOperator::ArrayJoin
        | JoinOperator::LeftArrayJoin
        | JoinOperator::InnerArrayJoin => op.clone(),
    })
}

/// Immutable reference to the [`JoinConstraint`] inside any variant that
/// carries one. Returns `None` for `CrossApply` / `OuterApply`.
#[must_use]
pub(crate) fn join_constraint_ref(op: &JoinOperator) -> Option<&JoinConstraint> {
    match op {
        JoinOperator::Join(c)
        | JoinOperator::Inner(c)
        | JoinOperator::Left(c)
        | JoinOperator::LeftOuter(c)
        | JoinOperator::Right(c)
        | JoinOperator::RightOuter(c)
        | JoinOperator::FullOuter(c)
        | JoinOperator::CrossJoin(c)
        | JoinOperator::Semi(c)
        | JoinOperator::LeftSemi(c)
        | JoinOperator::RightSemi(c)
        | JoinOperator::Anti(c)
        | JoinOperator::LeftAnti(c)
        | JoinOperator::RightAnti(c)
        | JoinOperator::StraightJoin(c)
        | JoinOperator::AsOf { constraint: c, .. } => Some(c),
        JoinOperator::CrossApply
        | JoinOperator::OuterApply
        | JoinOperator::ArrayJoin
        | JoinOperator::LeftArrayJoin
        | JoinOperator::InnerArrayJoin => None,
    }
}

/// Mutable reference to the [`JoinConstraint`] inside any variant that
/// carries one. Returns `None` for `CrossApply` / `OuterApply`.
pub(crate) fn join_constraint_mut(op: &mut JoinOperator) -> Option<&mut JoinConstraint> {
    match op {
        JoinOperator::Join(c)
        | JoinOperator::Inner(c)
        | JoinOperator::Left(c)
        | JoinOperator::LeftOuter(c)
        | JoinOperator::Right(c)
        | JoinOperator::RightOuter(c)
        | JoinOperator::FullOuter(c)
        | JoinOperator::CrossJoin(c)
        | JoinOperator::Semi(c)
        | JoinOperator::LeftSemi(c)
        | JoinOperator::RightSemi(c)
        | JoinOperator::Anti(c)
        | JoinOperator::LeftAnti(c)
        | JoinOperator::RightAnti(c)
        | JoinOperator::StraightJoin(c)
        | JoinOperator::AsOf { constraint: c, .. } => Some(c),
        JoinOperator::CrossApply
        | JoinOperator::OuterApply
        | JoinOperator::ArrayJoin
        | JoinOperator::LeftArrayJoin
        | JoinOperator::InnerArrayJoin => None,
    }
}

pub(crate) fn translate_join_operator<D: TranslationDirection>(
    join_operator: &JoinOperator,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<JoinOperator, Error> {
    map_join_operator(
        join_operator,
        &|c| translate_join_constraint::<D>(c, schema, options),
        &|e| D::translate_expr(e, schema, options),
    )
}

pub(crate) fn translate_join_constraint<D: TranslationDirection>(
    constraint: &JoinConstraint,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<JoinConstraint, Error> {
    Ok(match constraint {
        JoinConstraint::On(expr) => JoinConstraint::On(D::translate_expr(expr, schema, options)?),
        JoinConstraint::Using(idents) => JoinConstraint::Using(idents.clone()),
        JoinConstraint::Natural => JoinConstraint::Natural,
        JoinConstraint::None => JoinConstraint::None,
    })
}

/// Returns `true` when a derived subquery contains no FROM clause and no
/// column references, making it safe to drop a LATERAL keyword.
///
/// SQLite has no LATERAL join. A correlated lateral cannot be expressed and
/// a derived table referencing an outer column would fail at runtime with
/// "no such column". We only drop LATERAL when the subquery is trivially
/// self-contained: its body is a plain SELECT with an empty FROM list and
/// there are no Identifier or CompoundIdentifier nodes anywhere inside it.
///
/// The pattern follows `array.rs::references_a_column`, which uses the same
/// `visit_expressions` walk to detect column references in UNNEST operands.
fn subquery_is_trivially_uncorrelated(query: &Query) -> bool {
    let from_is_empty = match query.body.as_ref() {
        SetExpr::Select(sel) => sel.from.is_empty(),
        _ => false,
    };
    if !from_is_empty {
        return false;
    }
    !visit_expressions(query, |expr| {
        if matches!(expr, Expr::Identifier(_) | Expr::CompoundIdentifier(_)) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
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
            // generate_series with args parses as TableFactor::Table (not Function).
            if D::IS_FORWARD && args.is_some() && is_generate_series_object_name(name) {
                return Err(generate_series_not_supported_error());
            }
            if D::IS_FORWARD && sample.is_some() {
                return Err(Error::UnsupportedSQLiteFeature(
                    "TABLESAMPLE is not supported in SQLite. \
                     Use ORDER BY random() LIMIT n as an approximation."
                        .to_string(),
                ));
            }
            if D::IS_FORWARD && *with_ordinality {
                return Err(with_ordinality_not_supported_error());
            }
            // A function used where a table goes parses as `Table` carrying
            // args, not as `Function`, which is why the generate_series guard
            // above is duplicated in both arms. Anything with arguments here is
            // therefore a set-returning function rather than a relation.
            if D::IS_FORWARD
                && let Some(args) = args.as_ref()
            {
                return crate::impls::translator_impls::array::translate_set_returning_factor(
                    name,
                    &args.args,
                    alias.as_ref(),
                    schema,
                    options,
                );
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
            if D::IS_FORWARD {
                // SQLite grammar has no column list on a table alias.
                // The same limitation forces the derived-table shape in
                // array.rs::translate_unnest_factor.
                if alias.as_ref().is_some_and(|a| !a.columns.is_empty()) {
                    return Err(Error::UnsupportedSQLiteFeature(
                        "Table alias with a column list (AS alias(col1, col2, ...)) is not \
                         supported in SQLite grammar. Project the column names instead, for \
                         example: SELECT column1 AS a FROM (VALUES (1),(2)) AS v"
                            .to_string(),
                    ));
                }
                // SQLite has no LATERAL join. Drop the keyword only when the
                // subquery is trivially uncorrelated (no FROM clause, no column
                // references). Any other case would fail at runtime with
                // "no such column" because the outer scope is invisible.
                if *lateral && !subquery_is_trivially_uncorrelated(subquery) {
                    return Err(Error::UnsupportedSQLiteFeature(
                        "LATERAL on a correlated subquery is not supported in SQLite. SQLite \
                         has no LATERAL join. A correlated lateral cannot be expressed and a \
                         derived table would fail at runtime with no such column."
                            .to_string(),
                    ));
                }
                if sample.is_some() {
                    return Err(Error::UnsupportedSQLiteFeature(
                        "TABLESAMPLE is not supported in SQLite. \
                         Use ORDER BY random() LIMIT n as an approximation."
                            .to_string(),
                    ));
                }
            }
            TableFactor::Derived {
                subquery: Box::new(D::translate_query(subquery, schema, options)?),
                // Drop LATERAL; uncorrelated subqueries are safe without it and
                // correlated ones are rejected above.
                lateral: false,
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
        TableFactor::Function { lateral, name, args, with_ordinality, alias } => {
            if D::IS_FORWARD && is_generate_series_object_name(name) {
                return Err(generate_series_not_supported_error());
            }
            if D::IS_FORWARD && *with_ordinality {
                return Err(with_ordinality_not_supported_error());
            }
            if D::IS_FORWARD {
                return crate::impls::translator_impls::array::translate_set_returning_factor(
                    name,
                    args,
                    alias.as_ref(),
                    schema,
                    options,
                );
            }
            TableFactor::Function {
                lateral: *lateral,
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| translate_function_arg::<D>(arg, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                with_ordinality: *with_ordinality,
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
            // SQLite has no UNNEST; forward translation lowers it onto
            // `json_each`. Reverse translation leaves it alone.
            if D::IS_FORWARD {
                return crate::impls::translator_impls::array::translate_unnest_factor(
                    array_exprs,
                    alias.as_ref(),
                    *with_offset,
                    *with_ordinality,
                    schema,
                    options,
                );
            }
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

#[cfg(all(test, feature = "std"))]
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
