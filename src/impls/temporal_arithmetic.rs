//! PostgreSQL date and time arithmetic that carries no INTERVAL operand.
//!
//! SQLite holds `date`, `timestamp` and `time` as text, so `+` and `-` over
//! them reach it as arithmetic on whatever number the text starts with:
//! `date '2026-08-07' - date '2026-08-01'` answers 0 and `date '2026-08-01' +
//! 7` answers 2033. This module resolves the operand types PostgreSQL would
//! have resolved, rewrites the shapes SQLite can express, and refuses the rest
//! rather than emitting arithmetic over text.
//!
//! The set of shapes is closed rather than guessed: it is `pg_operator` for
//! `+` and `-` over `date`, `timestamp`, `timestamptz`, `time`, `timetz` and
//! `interval`, minus everything the INTERVAL rewrite already owns.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{boxed::Box, format, string::ToString, vec};

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, CastKind, DataType, DateTimeField, Expr, ExtractSyntax, Function, Value,
    ValueWithSpan,
};

use super::{
    datetime_helpers::build_subsecond_unixepoch_call,
    function_helpers::{simple_function_expr, string_literal},
    shared_helpers::{function_argument_exprs, is_integral_expression, unanimous_declared},
};
use crate::{
    errors::Error,
    prelude::{Pg2SqliteOptions, ReverseTranslator},
    traits::Translator,
};

/// The temporal type of an operand, as far as it resolves without
/// PostgreSQL's own type resolver.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemporalKind {
    Date,
    /// `timestamp` and `timestamptz` alike: nothing here separates them.
    Timestamp,
    /// `time` and `timetz`.
    Time,
}

/// Resolve the temporal type of `expr` from its own spelling or from the
/// declared type of the column it names.
fn temporal_kind(expr: &Expr, schema: &ParserDB) -> Option<TemporalKind> {
    match expr {
        Expr::Nested(inner) => temporal_kind(inner, schema),
        Expr::TypedString(typed) => kind_of_data_type(&typed.data_type),
        Expr::Cast { data_type, .. } => kind_of_data_type(data_type),
        // Either operation answers a timestamp, aware or naive.
        Expr::AtTimeZone { .. } => Some(TemporalKind::Timestamp),
        Expr::Function(function) => {
            kind_of_nullary(&crate::impls::object_name::last_ident(&function.name)?.value)
        }
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            unanimous_declared(expr, schema, kind_of_data_type)
        }
        // A rewritten subexpression is an operand in its own right, so
        // `(d + 7) - d` resolves.
        Expr::BinaryOp { left, op, right } => binary_result_kind(left, op, right, schema),
        _ => None,
    }
}

fn kind_of_data_type(data_type: &DataType) -> Option<TemporalKind> {
    match data_type {
        DataType::Date => Some(TemporalKind::Date),
        DataType::Timestamp(_, _) | DataType::Datetime(_) => Some(TemporalKind::Timestamp),
        DataType::Time(_, _) => Some(TemporalKind::Time),
        _ => None,
    }
}

/// The clock and calendar functions PostgreSQL spells without arguments.
fn kind_of_nullary(name: &str) -> Option<TemporalKind> {
    match name.to_ascii_lowercase().as_str() {
        "current_date" => Some(TemporalKind::Date),
        "now"
        | "current_timestamp"
        | "localtimestamp"
        | "transaction_timestamp"
        | "statement_timestamp"
        | "clock_timestamp" => Some(TemporalKind::Timestamp),
        "current_time" | "localtime" => Some(TemporalKind::Time),
        _ => None,
    }
}

/// The temporal type `left op right` answers, for the combinations that answer
/// one at all. `date - date` is an integer and `timestamp - timestamp` is an
/// interval, so both resolve to `None`.
fn binary_result_kind(
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
) -> Option<TemporalKind> {
    if !matches!(op, BinaryOperator::Plus | BinaryOperator::Minus) {
        return None;
    }
    // An INTERVAL keeps the other operand's type, except that it widens a date
    // to a timestamp.
    let widen = |kind| if kind == TemporalKind::Date { TemporalKind::Timestamp } else { kind };
    if is_interval(right) {
        return temporal_kind(left, schema).map(widen);
    }
    if is_interval(left) && matches!(op, BinaryOperator::Plus) {
        return temporal_kind(right, schema).map(widen);
    }

    match (temporal_kind(left, schema), temporal_kind(right, schema)) {
        (Some(TemporalKind::Date), None) if is_integral_expression(right, schema) => {
            Some(TemporalKind::Date)
        }
        (None, Some(TemporalKind::Date))
            if matches!(op, BinaryOperator::Plus) && is_integral_expression(left, schema) =>
        {
            Some(TemporalKind::Date)
        }
        (Some(TemporalKind::Date), Some(TemporalKind::Time))
        | (Some(TemporalKind::Time), Some(TemporalKind::Date))
            if matches!(op, BinaryOperator::Plus) =>
        {
            Some(TemporalKind::Timestamp)
        }
        _ => None,
    }
}

fn is_interval(expr: &Expr) -> bool {
    match expr {
        Expr::Interval(_) => true,
        Expr::Nested(inner) => is_interval(inner),
        _ => false,
    }
}

/// Rewrite or refuse `left op right` when either operand resolves to a date, a
/// timestamp or a time.
///
/// `None` means the pair has nothing to do with this family and the caller's
/// ordinary arithmetic applies, which is what keeps numeric `+`/`-` untouched.
pub(crate) fn translate_temporal_binary_op(
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Option<Result<Expr, Error>> {
    if !matches!(op, BinaryOperator::Plus | BinaryOperator::Minus) {
        return None;
    }
    // The INTERVAL rewrite runs first and owns every shape it accepts. What it
    // leaves behind is an interval it cannot express, which refuses on its own.
    if is_interval(left) || is_interval(right) {
        return None;
    }
    let left_kind = temporal_kind(left, schema);
    let right_kind = temporal_kind(right, schema);
    if left_kind.is_none() && right_kind.is_none() {
        return None;
    }
    Some(rewrite(left, op, right, (left_kind, right_kind), schema, options))
}

fn rewrite(
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    kinds: (Option<TemporalKind>, Option<TemporalKind>),
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    let subtracting = matches!(op, BinaryOperator::Minus);
    match kinds {
        (Some(TemporalKind::Date), Some(TemporalKind::Date)) if subtracting => {
            Ok(day_difference(left.translate(schema, options)?, right.translate(schema, options)?))
        }
        (Some(TemporalKind::Date), None) if is_integral_expression(right, schema) => {
            Ok(shift_date(left.translate(schema, options)?, op, right.translate(schema, options)?))
        }
        (None, Some(TemporalKind::Date))
            if !subtracting && is_integral_expression(left, schema) =>
        {
            Ok(shift_date(
                right.translate(schema, options)?,
                &BinaryOperator::Plus,
                left.translate(schema, options)?,
            ))
        }
        (Some(_), Some(_)) if subtracting => {
            Err(Error::UnsupportedSQLiteFeature(format!(
                "`{left} - {right}` yields an interval in PostgreSQL, and SQLite has no interval \
             type, so the difference has no value to hold. Wrap it in extract(epoch from ...) \
             for the difference in seconds, or subtract two dates for whole days."
            )))
        }
        (Some(_), Some(_)) => {
            Err(Error::UnsupportedSQLiteFeature(format!(
                "`{left} + {right}` pairs a date with a time of day, which PostgreSQL answers as a \
             timestamp, and SQLite has no operator that combines the two. Store one timestamp \
             column, or select the date and the time separately."
            )))
        }
        _ => {
            Err(Error::UnsupportedSQLiteFeature(format!(
                "`{left} {op} {right}` is date or time arithmetic over an operand this translation \
             cannot resolve. SQLite shifts a date only by a whole number of days and a timestamp \
             only by an INTERVAL, and neither is established here. Cast the operand to say which \
             it is."
            )))
        }
    }
}

/// `a - b` over two dates, which PostgreSQL answers in whole days.
///
/// Both operands land on a midday-anchored Julian day, so the difference is an
/// exact integer already and the cast only changes how it is held.
fn day_difference(left: Expr, right: Expr) -> Expr {
    Expr::Cast {
        expr: Box::new(Expr::BinaryOp {
            left: Box::new(simple_function_expr("julianday", vec![left], None)),
            op: BinaryOperator::Minus,
            right: Box::new(simple_function_expr("julianday", vec![right], None)),
        }),
        data_type: DataType::Integer(None),
        format: None,
        kind: CastKind::Cast,
    }
}

/// `date +/- n`, as a Julian day moved by whole days.
///
/// The obvious `date(a, printf('%+d days', n))` is silently wrong for a NULL
/// count: printf writes `+0 days` and the date comes back unchanged where
/// PostgreSQL answers NULL. Julian days propagate NULL from either side and
/// need no sign juggling for the subtracting case.
fn shift_date(date: Expr, op: &BinaryOperator, days: Expr) -> Expr {
    simple_function_expr(
        "date",
        vec![Expr::BinaryOp {
            left: Box::new(simple_function_expr("julianday", vec![date], None)),
            op: op.clone(),
            right: Box::new(days),
        }],
        None,
    )
}

/// The seconds in a difference PostgreSQL answers as an interval, which is what
/// `extract(epoch from (a - b))` and `date_part('epoch', a - b)` ask for.
///
/// `None` means the operand is not such a difference and the caller's ordinary
/// epoch translation applies.
///
/// `unixepoch(x, 'subsec')` is exact and carries the fraction. The julianday
/// form answers 525600.000013411 for a span PostgreSQL answers as 525600.
pub(crate) fn epoch_of_temporal_difference(
    expr: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Option<Result<Expr, Error>> {
    let mut operand = expr;
    while let Expr::Nested(inner) = operand {
        operand = inner;
    }
    let Expr::BinaryOp { left, op: BinaryOperator::Minus, right } = operand else {
        return None;
    };
    let (left_kind, right_kind) = (temporal_kind(left, schema)?, temporal_kind(right, schema)?);
    // `date - date` is an integer, and PostgreSQL refuses extract() over one.
    if left_kind == TemporalKind::Date && right_kind == TemporalKind::Date {
        return None;
    }
    Some(subsec_epoch_difference(left, right, schema, options))
}

fn subsec_epoch_difference(
    left: &Expr,
    right: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    // `'subsec'` is what makes `unixepoch` answer a float rather than whole
    // seconds, so it carries the fraction AND keeps the difference out of
    // integer division. Dropping it would make `extract(epoch from (a - b)) /
    // 60` divide as integers where PostgreSQL divides a numeric.
    Ok(Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(build_subsecond_unixepoch_call(left.translate(schema, options)?)),
        op: BinaryOperator::Minus,
        right: Box::new(build_subsecond_unixepoch_call(right.translate(schema, options)?)),
    })))
}

/// `to_timestamp(e)`: the instant `e` seconds after the epoch, rendered the
/// way PostgreSQL renders it.
///
/// `'subsec'` is what keeps the fraction, and it always renders three decimal
/// places, so `to_timestamp(1709647629)` would come out `...09.000` where
/// PostgreSQL writes `...09`. Trimming the trailing zeros and then the point
/// answers both cases, and the point stops the trim from eating a zero second.
/// Equality against a translated timestamp literal, which keeps its own
/// spelling, needs the two texts to agree.
pub(crate) fn subsecond_timestamp_from_epoch(epoch: Expr) -> Expr {
    let rendered = simple_function_expr(
        "datetime",
        vec![epoch, string_literal("unixepoch"), string_literal("subsec")],
        None,
    );
    trim_trailing_zeros(rendered)
}

/// `rtrim(rtrim(x, '0'), '.')`, which turns `09.500` into `09.5` and `09.000`
/// into `09`.
fn trim_trailing_zeros(rendered: Expr) -> Expr {
    let without_zeros = simple_function_expr("rtrim", vec![rendered, string_literal("0")], None);
    simple_function_expr("rtrim", vec![without_zeros, string_literal(".")], None)
}

/// Restore the PostgreSQL spelling of whatever this module emitted.
///
/// `None` when `expr` is not one of the three shapes, so anything else keeps
/// the ordinary reverse translation. Reversing here rather than in the reverse
/// translator keeps each emission and its inverse in one file: a change to one
/// that forgets the other is visible on the screen.
///
/// The four, in the order they appear below: a day count between two dates, a
/// date moved by whole days, the seconds in a difference PostgreSQL answers as
/// an interval, and an instant built from an epoch.
pub(crate) fn reverse_temporal_arithmetic(
    expr: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Option<Result<Expr, Error>> {
    if let Expr::Cast {
        expr: inner, data_type: DataType::Integer(None), kind: CastKind::Cast, ..
    } = expr
        && let Expr::BinaryOp { left, op: BinaryOperator::Minus, right } = inner.as_ref()
        && let (Some(from), Some(subtracted)) =
            (julianday_argument(left), julianday_argument(right))
    {
        return Some(reversed_binary_op(from, &BinaryOperator::Minus, subtracted, schema, options));
    }

    if let Expr::Function(function) = expr
        && named(function, "date")
        && let [
            Expr::BinaryOp { left, op: op @ (BinaryOperator::Plus | BinaryOperator::Minus), right },
        ] = function_argument_exprs(&function.args).as_slice()
        && let Some(date) = julianday_argument(left)
    {
        return Some(reversed_binary_op(date, op, right, schema, options));
    }

    let mut difference = expr;
    while let Expr::Nested(inner) = difference {
        difference = inner;
    }
    if let Expr::BinaryOp { left, op: BinaryOperator::Minus, right } = difference
        && let (Some(from), Some(subtracted)) =
            (subsec_epoch_argument(left), subsec_epoch_argument(right))
    {
        return Some(reversed_epoch_difference(from, subtracted, schema, options));
    }

    if let Some(epoch) = trimmed_subsecond_datetime_argument(expr) {
        return Some(reversed_to_timestamp(epoch, schema, options));
    }

    None
}

/// The sole argument of `julianday(x)`.
fn julianday_argument(expr: &Expr) -> Option<&Expr> {
    let Expr::Function(function) = expr else { return None };
    if !named(function, "julianday") {
        return None;
    }
    match function_argument_exprs(&function.args).as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

/// The value argument of `unixepoch(x, 'subsec')`.
fn subsec_epoch_argument(expr: &Expr) -> Option<&Expr> {
    let Expr::Function(function) = expr else { return None };
    if !named(function, "unixepoch") {
        return None;
    }
    match function_argument_exprs(&function.args).as_slice() {
        [value, Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(modifier), .. })]
            if modifier == "subsec" =>
        {
            Some(value)
        }
        _ => None,
    }
}

/// The epoch argument of `rtrim(rtrim(datetime(e, 'unixepoch', 'subsec'), '0'),
/// '.')`.
fn trimmed_subsecond_datetime_argument(expr: &Expr) -> Option<&Expr> {
    let inner = rtrim_argument(expr, ".")?;
    let rendered = rtrim_argument(inner, "0")?;
    let Expr::Function(function) = rendered else { return None };
    if !named(function, "datetime") {
        return None;
    }
    match function_argument_exprs(&function.args).as_slice() {
        [
            epoch,
            Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(unixepoch), .. }),
            Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(subsec), .. }),
        ] if unixepoch == "unixepoch" && subsec == "subsec" => Some(epoch),
        _ => None,
    }
}

/// The first argument of `rtrim(x, <cut>)`.
fn rtrim_argument<'a>(expr: &'a Expr, cut: &str) -> Option<&'a Expr> {
    let Expr::Function(function) = expr else { return None };
    if !named(function, "rtrim") {
        return None;
    }
    match function_argument_exprs(&function.args).as_slice() {
        [value, Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(set), .. })]
            if set == cut =>
        {
            Some(value)
        }
        _ => None,
    }
}

/// `to_timestamp(e)` over the reversed epoch.
fn reversed_to_timestamp(
    epoch: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    Ok(simple_function_expr(
        "to_timestamp",
        vec![ReverseTranslator::reverse_translate(epoch, schema, options)?],
        None,
    ))
}

fn named(function: &Function, name: &str) -> bool {
    crate::impls::object_name::last_ident(&function.name)
        .is_some_and(|ident| ident.value.eq_ignore_ascii_case(name))
}

fn reversed_binary_op(
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    Ok(Expr::BinaryOp {
        left: Box::new(left.reverse_translate(schema, options)?),
        op: op.clone(),
        right: Box::new(right.reverse_translate(schema, options)?),
    })
}

fn reversed_epoch_difference(
    left: &Expr,
    right: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    Ok(Expr::Extract {
        field: DateTimeField::Epoch,
        syntax: ExtractSyntax::From,
        expr: Box::new(Expr::Nested(Box::new(reversed_binary_op(
            left,
            &BinaryOperator::Minus,
            right,
            schema,
            options,
        )?))),
    })
}
