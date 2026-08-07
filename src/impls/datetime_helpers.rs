//! Shared date/time mapping helpers for forward and reverse translation.

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

use sqlparser::ast::{BinaryOperator, CastKind, DataType, DateTimeField, Expr};

use super::function_helpers::{integer_literal, simple_function_expr, string_literal};

/// Canonical date/time part keys used for shared mappings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DatePartKey {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
    DayOfWeek,
    DayOfYear,
    Epoch,
    /// ISO 8601 week number, Monday based, week 1 holding the first Thursday.
    Week,
    /// The year that ISO week belongs to, which differs from the calendar year
    /// at a boundary: 2023-01-01 is week 52 of 2022.
    IsoYear,
    /// ISO weekday, Monday as 1 through Sunday as 7, where `DayOfWeek` counts
    /// Sunday as 0.
    IsoDayOfWeek,
}

/// Parse a PostgreSQL `date_part` / `extract` textual field into a canonical
/// key.
#[must_use]
pub(crate) fn parse_date_part_key(field: &str) -> Option<DatePartKey> {
    match field.to_ascii_lowercase().as_str() {
        "year" | "years" => Some(DatePartKey::Year),
        "month" | "months" => Some(DatePartKey::Month),
        "day" | "days" => Some(DatePartKey::Day),
        "hour" | "hours" => Some(DatePartKey::Hour),
        "minute" | "minutes" => Some(DatePartKey::Minute),
        "second" | "seconds" => Some(DatePartKey::Second),
        "dow" | "weekday" => Some(DatePartKey::DayOfWeek),
        "doy" => Some(DatePartKey::DayOfYear),
        "epoch" => Some(DatePartKey::Epoch),
        "week" | "weeks" => Some(DatePartKey::Week),
        "isoyear" => Some(DatePartKey::IsoYear),
        "isodow" => Some(DatePartKey::IsoDayOfWeek),
        _ => None,
    }
}

/// Convert a parsed [`DateTimeField`] to a canonical key.
#[must_use]
pub(crate) fn datetime_field_key(field: &DateTimeField) -> Option<DatePartKey> {
    match field {
        DateTimeField::Year | DateTimeField::Years => Some(DatePartKey::Year),
        DateTimeField::Month | DateTimeField::Months => Some(DatePartKey::Month),
        DateTimeField::Day | DateTimeField::Days => Some(DatePartKey::Day),
        DateTimeField::Hour | DateTimeField::Hours => Some(DatePartKey::Hour),
        DateTimeField::Minute | DateTimeField::Minutes => Some(DatePartKey::Minute),
        DateTimeField::Second | DateTimeField::Seconds => Some(DatePartKey::Second),
        // PostgreSQL EXTRACT(DOW/DOY ...) uses Dow/Doy variants, not the
        // MySQL-style DayOfWeek/DayOfYear variants. Map both so either form works.
        DateTimeField::Dow | DateTimeField::DayOfWeek => Some(DatePartKey::DayOfWeek),
        DateTimeField::Doy | DateTimeField::DayOfYear => Some(DatePartKey::DayOfYear),
        DateTimeField::Epoch => Some(DatePartKey::Epoch),
        DateTimeField::Week(_) | DateTimeField::Weeks | DateTimeField::IsoWeek => {
            Some(DatePartKey::Week)
        }
        DateTimeField::Isoyear => Some(DatePartKey::IsoYear),
        DateTimeField::Isodow => Some(DatePartKey::IsoDayOfWeek),
        _ => None,
    }
}

/// Convert a canonical key to SQLite `strftime` format and cast type.
#[must_use]
pub(crate) fn strftime_mapping_for_key(key: DatePartKey) -> (&'static str, DataType) {
    match key {
        DatePartKey::Year => ("%Y", DataType::Integer(None)),
        DatePartKey::Month => ("%m", DataType::Integer(None)),
        DatePartKey::Day => ("%d", DataType::Integer(None)),
        DatePartKey::Hour => ("%H", DataType::Integer(None)),
        DatePartKey::Minute => ("%M", DataType::Integer(None)),
        DatePartKey::Second => ("%f", DataType::Real),
        DatePartKey::DayOfWeek => ("%w", DataType::Integer(None)),
        DatePartKey::DayOfYear => ("%j", DataType::Integer(None)),
        DatePartKey::Epoch => ("%s", DataType::Real),
        // %V is the ISO week, Monday based, where %W is Sunday based and
        // disagrees at every year boundary. Same for %G against %Y and %u
        // against %w.
        DatePartKey::Week => ("%V", DataType::Integer(None)),
        DatePartKey::IsoYear => ("%G", DataType::Integer(None)),
        DatePartKey::IsoDayOfWeek => ("%u", DataType::Integer(None)),
    }
}

/// Parse a SQLite `strftime` format into a PostgreSQL date-time field.
#[must_use]
pub(crate) fn datetime_field_from_strftime_format(format: &str) -> Option<DateTimeField> {
    match format {
        "%Y" => Some(DateTimeField::Year),
        "%m" => Some(DateTimeField::Month),
        "%d" => Some(DateTimeField::Day),
        "%H" => Some(DateTimeField::Hour),
        "%M" => Some(DateTimeField::Minute),
        // %S is standard strftime; %f is emitted for fractional-second paths.
        "%S" | "%f" => Some(DateTimeField::Second),
        "%s" => Some(DateTimeField::Epoch),
        // %W, the Sunday based week, has no PostgreSQL field: EXTRACT(WEEK)
        // is the ISO one, so reversing %W to it would change the answer.
        "%V" => Some(DateTimeField::Week(None)),
        "%G" => Some(DateTimeField::Isoyear),
        "%u" => Some(DateTimeField::Isodow),
        "%w" => Some(DateTimeField::DayOfWeek),
        "%j" => Some(DateTimeField::DayOfYear),
        _ => None,
    }
}

/// Build `strftime('<format>', <expr>)`.
#[must_use]
pub(crate) fn build_strftime_call(format: &str, value_expr: Expr) -> Expr {
    simple_function_expr("strftime", vec![string_literal(format), value_expr], None)
}

fn binary(left: Expr, op: BinaryOperator, right: Expr) -> Expr {
    Expr::BinaryOp { left: Box::new(left), op, right: Box::new(right) }
}

/// `CAST(strftime('<format>', <expr>) AS INTEGER)`, a calendar component as a
/// number rather than as text.
fn strftime_number(format: &str, value_expr: Expr) -> Expr {
    Expr::Cast {
        expr: Box::new(build_strftime_call(format, value_expr)),
        data_type: DataType::Integer(None),
        format: None,
        kind: CastKind::Cast,
    }
}

/// The Monday of the ISO week `value_expr` falls in, at midnight, which is
/// what PostgreSQL's `date_trunc('week', ...)` answers.
///
/// SQLite's `weekday 1` modifier moves forward to the next Monday and stays
/// put when the date is already one. Stepping back six days first therefore
/// makes the current week's Monday the next one in every case, the date itself
/// included. Checked against PostgreSQL 16 on nine dates covering Sundays,
/// Mondays, and year boundaries.
#[must_use]
pub(crate) fn build_date_trunc_week_call(value_expr: Expr) -> Expr {
    simple_function_expr(
        "datetime",
        vec![
            value_expr,
            string_literal("-6 days"),
            string_literal("weekday 1"),
            string_literal("start of day"),
        ],
        None,
    )
}

/// The first day of the quarter `value_expr` falls in, at midnight.
///
/// `((month - 1) / 3) * 3` is the count of whole months from January to the
/// start of that quarter, using SQLite's truncating integer division.
///
/// The arithmetic is parenthesised because SQLite binds `||` tighter than `*`
/// and `/`, so a flat rendering would group the operands the wrong way, and
/// `Display` adds no parentheses of its own.
#[must_use]
pub(crate) fn build_date_trunc_quarter_call(value_expr: Expr) -> Expr {
    let month_index = binary(
        strftime_number("%m", value_expr.clone()),
        BinaryOperator::Minus,
        integer_literal(1),
    );
    let months_into_year = binary(
        binary(Expr::Nested(Box::new(month_index)), BinaryOperator::Divide, integer_literal(3)),
        BinaryOperator::Multiply,
        integer_literal(3),
    );
    let modifier = binary(
        binary(
            string_literal("+"),
            BinaryOperator::StringConcat,
            Expr::Nested(Box::new(months_into_year)),
        ),
        BinaryOperator::StringConcat,
        string_literal(" months"),
    );

    simple_function_expr(
        "datetime",
        vec![value_expr, string_literal("start of year"), modifier],
        None,
    )
}

/// The first day of the `span`-year period `value_expr` falls in, at midnight.
///
/// `offset` is where the count starts, and it is the whole difference between
/// the three PostgreSQL units this serves. A decade floors the year, so 2000
/// begins the decade 2000 and `offset` is 0. A century and a millennium count
/// from year 1, so 2000 belongs to the century beginning 1901 and the
/// millennium beginning 1001, and `offset` is 1. All three verified against
/// PostgreSQL 16.
///
/// `printf` pads the year, so a period beginning before year 1000 still forms
/// a date SQLite can read.
///
/// The subtraction is parenthesised because `*` and `/` bind tighter than `-`,
/// so a flat `y - 1 / 100 * 100` would reduce to `y - 0`. When `offset` is
/// zero both terms are dropped rather than emitted as `- 0` and `+ 0`.
#[must_use]
pub(crate) fn build_date_trunc_year_span_call(value_expr: Expr, span: i64, offset: i64) -> Expr {
    let year = strftime_number("%Y", value_expr);
    let counted_from = if offset == 0 {
        year
    } else {
        Expr::Nested(Box::new(binary(year, BinaryOperator::Minus, integer_literal(offset))))
    };

    let floored = binary(
        binary(counted_from, BinaryOperator::Divide, integer_literal(span)),
        BinaryOperator::Multiply,
        integer_literal(span),
    );
    let period_start = if offset == 0 {
        floored
    } else {
        binary(floored, BinaryOperator::Plus, integer_literal(offset))
    };

    simple_function_expr(
        "datetime",
        vec![simple_function_expr(
            "printf",
            vec![string_literal("%04d-01-01 00:00:00"), period_start],
            None,
        )],
        None,
    )
}
