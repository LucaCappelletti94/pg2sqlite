//! Shared timezone helpers for forward and reverse translation.
//!
//! Both directions have to agree about the sign convention that separates the
//! two `AT TIME ZONE` operations, so the rule and the type resolution it needs
//! live here rather than in either direction.

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
use sqlparser::ast::{DataType, Expr, TimezoneInfo};

use crate::impls::shared_helpers::every_declared_type_matches;

/// Returns `true` when `value` is a fixed UTC offset in `+HH:MM` / `-HH:MM`
/// format.
#[must_use]
pub(crate) fn is_fixed_utc_offset(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 6 || (bytes[0] != b'+' && bytes[0] != b'-') || bytes[3] != b':' {
        return false;
    }
    if !bytes[1].is_ascii_digit()
        || !bytes[2].is_ascii_digit()
        || !bytes[4].is_ascii_digit()
        || !bytes[5].is_ascii_digit()
    {
        return false;
    }

    let hour = (bytes[1] - b'0') * 10 + (bytes[2] - b'0');
    let minute = (bytes[4] - b'0') * 10 + (bytes[5] - b'0');

    hour <= 23 && minute <= 59
}

fn normalize_timezone_modifier(
    value: &str,
    normalized_utc: &'static str,
    normalized_local: &'static str,
) -> Option<String> {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();

    match lower.as_str() {
        "utc" | "gmt" | "z" => return Some(normalized_utc.to_string()),
        "local" | "localtime" => return Some(normalized_local.to_string()),
        _ => {}
    }

    if is_fixed_utc_offset(trimmed) {
        return Some(trimmed.to_string());
    }

    for prefix in ["utc", "gmt"] {
        if let Some(rest) = lower.strip_prefix(prefix)
            && is_fixed_utc_offset(rest)
        {
            return Some(rest.to_string());
        }
    }

    None
}

/// Normalizes PostgreSQL `AT TIME ZONE` literals to SQLite datetime modifiers.
#[must_use]
pub(crate) fn normalize_timezone_modifier_for_sqlite(value: &str) -> Option<String> {
    normalize_timezone_modifier(value, "utc", "localtime")
}

/// Normalizes SQLite datetime timezone modifiers to PostgreSQL
/// `AT TIME ZONE` literals.
#[must_use]
pub(crate) fn normalize_timezone_modifier_for_postgres(value: &str) -> Option<String> {
    normalize_timezone_modifier(value, "UTC", "localtime")
}

/// Whether a timestamp expression carries a zone, which decides which way
/// `AT TIME ZONE` shifts it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimestampAwareness {
    /// A bare `timestamp`, read as local to the named zone.
    Naive,
    /// A `timestamptz`, already an instant, converted into the named zone.
    Aware,
}

/// Resolve whether `expr` is a `timestamptz`, from its own spelling or from the
/// declared type of the column it names.
///
/// Both directions need this and must agree: the forward direction negates an
/// aware operand's offset, so the reverse direction has to negate it back.
#[must_use]
pub(crate) fn timestamp_awareness(expr: &Expr, schema: &ParserDB) -> Option<TimestampAwareness> {
    fn from_data_type(data_type: &DataType) -> Option<TimestampAwareness> {
        match data_type {
            DataType::Timestamp(_, TimezoneInfo::Tz | TimezoneInfo::WithTimeZone) => {
                Some(TimestampAwareness::Aware)
            }
            DataType::Timestamp(_, _) | DataType::Date => Some(TimestampAwareness::Naive),
            _ => None,
        }
    }

    match expr {
        Expr::Nested(inner) => timestamp_awareness(inner, schema),
        Expr::TypedString(typed) => from_data_type(&typed.data_type),
        Expr::Cast { data_type, .. } => from_data_type(data_type),
        // `AT TIME ZONE` inverts what it is given, so a nested one resolves.
        // Measured with `pg_typeof` on PostgreSQL 16: a bare timestamp answers
        // `timestamp with time zone` and a timestamptz answers `timestamp
        // without time zone`, and the inversion composes over nesting.
        Expr::AtTimeZone { timestamp, .. } => {
            match timestamp_awareness(timestamp, schema)? {
                TimestampAwareness::Naive => Some(TimestampAwareness::Aware),
                TimestampAwareness::Aware => Some(TimestampAwareness::Naive),
            }
        }
        // PostgreSQL's now() and its aliases return timestamptz, and
        // localtimestamp returns a bare timestamp.
        Expr::Function(function) => {
            let name =
                crate::impls::object_name::last_ident(&function.name)?.value.to_ascii_lowercase();
            match name.as_str() {
                "now"
                | "current_timestamp"
                | "transaction_timestamp"
                | "statement_timestamp"
                | "clock_timestamp" => Some(TimestampAwareness::Aware),
                "localtimestamp" => Some(TimestampAwareness::Naive),
                _ => None,
            }
        }
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            let declared_contains = |needle: &'static str| {
                every_declared_type_matches(expr, schema, |declared| {
                    declared.to_ascii_lowercase().contains(needle)
                })
            };
            if declared_contains("with time zone") || declared_contains("timestamptz") {
                Some(TimestampAwareness::Aware)
            } else if declared_contains("timestamp") || declared_contains("date") {
                Some(TimestampAwareness::Naive)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The same offset with its sign flipped, for the only zone spelling whose sign
/// differs between the two `AT TIME ZONE` operations: a fixed offset that
/// actually shifts. `None` for every other spelling, which both directions
/// carry through unchanged.
///
/// A zero offset is excluded because flipping it changes nothing, so neither
/// direction has to resolve the operand's type to handle it. Flipping is its
/// own inverse, which is what lets one function serve both directions.
#[must_use]
pub(crate) fn flipped_shifting_offset(modifier: &str) -> Option<String> {
    if !is_fixed_utc_offset(modifier) || &modifier[1..] == "00:00" {
        return None;
    }
    let flipped = if modifier.starts_with('-') { '+' } else { '-' };
    Some(format!("{flipped}{}", &modifier[1..]))
}
