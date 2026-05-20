//! Shared timezone parsing helpers for forward and reverse translation.

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
