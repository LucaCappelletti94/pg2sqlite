//! Validation and normalisation of PostgreSQL date, time and timestamp
//! literals written into a temporal column.
//!
//! A temporal column is stored as `TEXT`, so the text is the value: what
//! PostgreSQL refuses must be refused here, and what PostgreSQL rewrites must
//! be rewritten here. Two things depend on it. `'2024-13-01'` used to be
//! stored where PostgreSQL answers `date/time field value out of range`, and
//! `'2024-3-5'` used to be stored as written, which PostgreSQL prints as
//! `2024-03-05` and which SQLite's own date functions answer `NULL` for, so
//! an unpadded value was unusable by the very functions the translation emits.
//!
//! Only ISO 8601 input is translated. PostgreSQL reads `'1/2/2024'` and
//! `'Jan 2 2024'` as well, but the first depends on the server's `DateStyle`
//! and the second on its locale, neither of which a translator can see, and
//! `'today'` is resolved when the server reads it rather than standing for a
//! fixed date.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    format,
    string::{String, ToString},
};

use sqlparser::ast::{DataType, TimezoneInfo};

use crate::errors::Error;

/// The temporal type a literal is being written into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TemporalLiteralKind {
    /// `date`: a calendar day.
    Date,
    /// `time` and `time with time zone`.
    Time {
        /// Whether a UTC offset may follow the time.
        zoned: bool,
    },
    /// `timestamp` and `timestamp with time zone`.
    Timestamp {
        /// Whether a UTC offset may follow the time.
        zoned: bool,
    },
}

/// The kind of temporal literal `data_type` takes, or `None` for every other
/// type.
#[must_use]
pub(crate) fn temporal_literal_kind(data_type: &DataType) -> Option<TemporalLiteralKind> {
    let zoned = |info: &TimezoneInfo| matches!(info, TimezoneInfo::Tz | TimezoneInfo::WithTimeZone);
    match data_type {
        DataType::Date => Some(TemporalLiteralKind::Date),
        DataType::Time(_, info) => Some(TemporalLiteralKind::Time { zoned: zoned(info) }),
        DataType::Timestamp(_, info) => Some(TemporalLiteralKind::Timestamp { zoned: zoned(info) }),
        _ => None,
    }
}

/// The text PostgreSQL prints for `text` read as `kind`, or the refusal
/// PostgreSQL answers for it.
///
/// A timestamp with no time part takes midnight, which is what PostgreSQL
/// prints for `'2024-03-05'::timestamp`. A fractional second is kept as
/// written, since PostgreSQL prints the digits it was given.
pub(crate) fn normalize_temporal_literal(
    kind: TemporalLiteralKind,
    text: &str,
) -> Result<String, Error> {
    let trimmed = text.trim();
    reject_non_iso_input(kind, trimmed)?;
    match kind {
        TemporalLiteralKind::Date => {
            let (year, month, day) = parse_date(kind, trimmed, trimmed)?;
            Ok(format!("{year:04}-{month:02}-{day:02}"))
        }
        TemporalLiteralKind::Time { zoned } => {
            let (body, zone) = split_time_zone(kind, trimmed, zoned)?;
            Ok(format!("{}{zone}", parse_time(kind, body, trimmed)?))
        }
        TemporalLiteralKind::Timestamp { zoned } => {
            let (body, zone) = split_time_zone(kind, trimmed, zoned)?;
            let (date, time) = split_timestamp(body);
            let (mut year, mut month, mut day) = parse_date(kind, date, trimmed)?;
            let mut time = match time {
                Some(time) => parse_time(kind, time, trimmed)?,
                None => "00:00:00".to_string(),
            };
            // PostgreSQL takes 24:00:00 on a timestamp as the next day's
            // midnight: `'2024-03-05 24:00:00'` answers `2024-03-06
            // 00:00:00`. A `time` column keeps the hour, which is why the
            // roll is here rather than in `parse_time`.
            if time.starts_with("24:") {
                time = "00:00:00".to_string();
                (year, month, day) = next_day(year, month, day);
            }
            Ok(format!("{year:04}-{month:02}-{day:02} {time}{zone}"))
        }
    }
}

/// The name PostgreSQL gives the type in its own error messages.
fn type_name(kind: TemporalLiteralKind) -> &'static str {
    match kind {
        TemporalLiteralKind::Date => "date",
        TemporalLiteralKind::Time { zoned: false } => "time",
        TemporalLiteralKind::Time { zoned: true } => "time with time zone",
        TemporalLiteralKind::Timestamp { zoned: false } => "timestamp",
        TemporalLiteralKind::Timestamp { zoned: true } => "timestamp with time zone",
    }
}

/// The ISO shape each kind is written in, for a refusal to point at.
fn expected_shape(kind: TemporalLiteralKind) -> &'static str {
    match kind {
        TemporalLiteralKind::Date => "YYYY-MM-DD",
        TemporalLiteralKind::Time { zoned: false } => "HH:MM:SS",
        TemporalLiteralKind::Time { zoned: true } => "HH:MM:SS+HH:MM",
        TemporalLiteralKind::Timestamp { zoned: false } => "YYYY-MM-DD HH:MM:SS",
        TemporalLiteralKind::Timestamp { zoned: true } => "YYYY-MM-DD HH:MM:SS+HH:MM",
    }
}

/// The refusal for text that is not ISO 8601, naming what was written.
fn unreadable(kind: TemporalLiteralKind, text: &str, reason: &str) -> Error {
    Error::forward_refusal(format!(
        "invalid input syntax for type {}: \"{text}\": {reason} The replica stores a temporal \
         column as text, so a value it cannot read is a value it would hold verbatim. Write it \
         as {}.",
        type_name(kind),
        expected_shape(kind)
    ))
}

/// The refusal PostgreSQL answers for a component outside its range.
fn out_of_range(text: &str) -> Error {
    Error::forward_refusal(format!(
        "date/time field value out of range: \"{text}\". PostgreSQL refuses this value, so the \
         replica must not hold it."
    ))
}

/// Refuses the PostgreSQL input forms whose meaning is not in the text.
///
/// A keyword is resolved when the server reads it, so storing the word would
/// freeze a value that was meant to move. A slash-separated date is January 2
/// under the default `DateStyle` and February 1 under `DMY`. A month name
/// depends on the server's locale, and `AM`/`PM` is not ISO.
fn reject_non_iso_input(kind: TemporalLiteralKind, text: &str) -> Result<(), Error> {
    const KEYWORDS: &[&str] = &[
        "now",
        "today",
        "tomorrow",
        "yesterday",
        "epoch",
        "infinity",
        "-infinity",
        "+infinity",
        "allballs",
    ];
    let lowered = text.to_ascii_lowercase();
    if KEYWORDS.contains(&lowered.as_str()) {
        return Err(unreadable(
            kind,
            text,
            "PostgreSQL resolves this keyword when it reads it, so the replica would hold the \
             word rather than the value it stood for.",
        ));
    }
    if text.contains('/') {
        return Err(unreadable(
            kind,
            text,
            "a slash-separated date means different days under different DateStyle settings, \
             which is server state the translation cannot read.",
        ));
    }
    if lowered.ends_with(" am") || lowered.ends_with(" pm") {
        return Err(unreadable(kind, text, "a 12-hour clock reading is not ISO 8601."));
    }
    if text.chars().any(char::is_alphabetic)
        && !matches!(kind, TemporalLiteralKind::Date)
        && !text.chars().all(|c| {
            c.is_ascii_digit() || matches!(c, '-' | '+' | ':' | '.' | ' ' | 'T' | 'Z' | 'z')
        })
    {
        return Err(unreadable(kind, text, "a month name depends on the server's locale."));
    }
    if matches!(kind, TemporalLiteralKind::Date) && text.chars().any(char::is_alphabetic) {
        return Err(unreadable(kind, text, "a month name depends on the server's locale."));
    }
    Ok(())
}

/// Splits a UTC offset off the end, answering the normalised offset text.
///
/// `Z` is `+00:00`, and an offset written as hours only takes `:00` minutes,
/// which SQLite's date functions need: they answer `NULL` for `±HH`. The
/// search starts after the time part, since a date's own separators are
/// hyphens and `'2024-01-01'` would otherwise read `-01` as an offset.
fn split_time_zone(
    kind: TemporalLiteralKind,
    text: &str,
    zoned: bool,
) -> Result<(&str, String), Error> {
    let time_start = match kind {
        TemporalLiteralKind::Time { .. } => Some(0),
        _ => text.find([' ', 'T', 't']).map(|index| index + 1),
    };
    let offset_at =
        time_start.and_then(|start| text[start..].find(['+', '-']).map(|index| index + start));
    let (body, zone) = match offset_at {
        Some(index) => (&text[..index], Some(text[index..].to_string())),
        None => {
            match text.strip_suffix(['Z', 'z']) {
                Some(body) => (body, Some("+00:00".to_string())),
                None => (text, None),
            }
        }
    };
    let Some(zone) = zone else { return Ok((text, String::new())) };
    if !zoned {
        return Err(unreadable(
            kind,
            text,
            "this column has no time zone, so an offset in the value would be dropped.",
        ));
    }
    Ok((body.trim_end(), normalize_offset(kind, &zone, text)?))
}

/// Brings a UTC offset to `±HH:MM`, the only form SQLite's date functions
/// read.
fn normalize_offset(kind: TemporalLiteralKind, zone: &str, text: &str) -> Result<String, Error> {
    let (sign, digits) = zone.split_at(1);
    if !matches!(sign, "+" | "-") {
        return Err(unreadable(kind, text, "the time zone offset has no sign."));
    }
    let mut parts = digits.split(':');
    let hours: u32 = parse_component(kind, parts.next().unwrap_or_default(), text)?;
    let minutes: u32 = match parts.next() {
        Some(minutes) => parse_component(kind, minutes, text)?,
        None => 0,
    };
    if parts.next().is_some() {
        return Err(unreadable(kind, text, "the time zone offset has too many parts."));
    }
    if hours > 15 || minutes > 59 {
        return Err(out_of_range(text));
    }
    Ok(format!("{sign}{hours:02}:{minutes:02}"))
}

/// Splits a timestamp into its date and, when present, its time.
fn split_timestamp(text: &str) -> (&str, Option<&str>) {
    match text.find([' ', 'T', 't']) {
        Some(index) => {
            let time = text[index + 1..].trim();
            (&text[..index], if time.is_empty() { None } else { Some(time) })
        }
        None => (text, None),
    }
}

/// Parses `YYYY-MM-DD` with unpadded components allowed, validating the
/// calendar.
fn parse_date(kind: TemporalLiteralKind, date: &str, text: &str) -> Result<(u32, u32, u32), Error> {
    let mut parts = date.split('-');
    let year: u32 = parse_component(kind, parts.next().unwrap_or_default(), text)?;
    let month: u32 = parse_component(kind, parts.next().ok_or_else(|| shape(kind, text))?, text)?;
    let day: u32 = parse_component(kind, parts.next().ok_or_else(|| shape(kind, text))?, text)?;
    if parts.next().is_some() {
        return Err(shape(kind, text));
    }
    if !(1..=9999).contains(&year) {
        return Err(out_of_range(text));
    }
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return Err(out_of_range(text));
    }
    Ok((year, month, day))
}

/// Parses `HH:MM[:SS[.frac]]` with unpadded components allowed, answering the
/// text PostgreSQL prints.
fn parse_time(kind: TemporalLiteralKind, time: &str, text: &str) -> Result<String, Error> {
    let mut parts = time.split(':');
    let hour: u32 = parse_component(kind, parts.next().unwrap_or_default(), text)?;
    let minute: u32 = parse_component(kind, parts.next().ok_or_else(|| shape(kind, text))?, text)?;
    let (second, fraction) = match parts.next() {
        Some(second) => {
            match second.split_once('.') {
                Some((whole, fraction)) => {
                    if fraction.is_empty() || !fraction.chars().all(|c| c.is_ascii_digit()) {
                        return Err(shape(kind, text));
                    }
                    (parse_component(kind, whole, text)?, Some(fraction))
                }
                None => (parse_component(kind, second, text)?, None),
            }
        }
        None => (0, None),
    };
    if parts.next().is_some() {
        return Err(shape(kind, text));
    }
    if minute > 59 || second > 59 {
        // Second 60 is a leap second, which PostgreSQL normalises to the next
        // minute and SQLite cannot represent, so it is named rather than
        // folded into the generic range refusal.
        if second == 60 {
            return Err(Error::forward_refusal(format!(
                "\"{text}\" has second=60, which PostgreSQL normalises away and SQLite cannot \
                 represent. Write the value PostgreSQL would store instead."
            )));
        }
        return Err(out_of_range(text));
    }
    if hour > 24 || (hour == 24 && (minute > 0 || second > 0 || fraction.is_some())) {
        return Err(out_of_range(text));
    }
    Ok(match fraction {
        Some(fraction) => format!("{hour:02}:{minute:02}:{second:02}.{fraction}"),
        None => format!("{hour:02}:{minute:02}:{second:02}"),
    })
}

/// One numeric component of a temporal literal.
fn parse_component(kind: TemporalLiteralKind, part: &str, text: &str) -> Result<u32, Error> {
    if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
        return Err(shape(kind, text));
    }
    part.parse().map_err(|_| out_of_range(text))
}

/// The refusal for text whose shape is not the expected one.
fn shape(kind: TemporalLiteralKind, text: &str) -> Error {
    unreadable(kind, text, "this is not the ISO 8601 form of the type.")
}

/// The number of days in `month` of `year`, Gregorian.
fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

/// The day after `year-month-day`, Gregorian.
fn next_day(year: u32, month: u32, day: u32) -> (u32, u32, u32) {
    if day < days_in_month(year, month) {
        return (year, month, day + 1);
    }
    if month < 12 {
        return (year, month + 1, 1);
    }
    (year + 1, 1, 1)
}
