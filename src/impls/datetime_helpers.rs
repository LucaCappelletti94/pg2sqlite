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

use sqlparser::ast::{DataType, DateTimeField, Expr, WindowType};

use super::function_helpers::{simple_function_expr, string_literal};

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

/// Build `strftime('<format>', <expr>)` with optional window specification.
#[must_use]
pub(crate) fn build_strftime_call(
    format: &str,
    value_expr: Expr,
    over: Option<WindowType>,
) -> Expr {
    simple_function_expr("strftime", vec![string_literal(format), value_expr], over)
}
