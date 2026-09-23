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
/// A timestamp with no time part takes midnight. A `timestamp with time zone`
/// becomes the replica's one text per instant, UTC with microseconds and a
/// `+00:00` offset, so that equal instants compare as equal text.
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
            let zone = zone.map_or_else(String::new, |zone| zone.render());
            Ok(format!("{}{zone}", parse_time(kind, body, trimmed)?.render()))
        }
        TemporalLiteralKind::Timestamp { zoned } => {
            let (body, zone) = split_time_zone(kind, trimmed, zoned)?;
            let (date, time) = split_timestamp(body);
            let (year, month, day) = parse_date(kind, date, trimmed)?;
            let clock = match time {
                Some(time) => parse_time(kind, time, trimmed)?,
                None => Clock::MIDNIGHT,
            };
            if zoned {
                // No offset reads as UTC, the zone the replica's own clock
                // answers in.
                let offset = zone.map_or(0, |zone| zone.seconds());
                return canonical_instant((year, month, day), &clock, offset, trimmed);
            }
            // PostgreSQL reads 24:00:00 on a timestamp as the next day's
            // midnight.
            if clock.hour == 24 {
                let (year, month, day) = next_day(year, month, day);
                return Ok(format!("{year:04}-{month:02}-{day:02} {}", Clock::MIDNIGHT.render()));
            }
            Ok(format!("{year:04}-{month:02}-{day:02} {}", clock.render()))
        }
    }
}

/// The canonical `YYYY-MM-DD HH:MM:SS.ffffff+00:00` text of an instant.
fn canonical_instant(
    (year, month, day): (u32, u32, u32),
    clock: &Clock<'_>,
    offset_seconds: i64,
    text: &str,
) -> Result<String, Error> {
    const MICROS_PER_SECOND: i64 = 1_000_000;
    const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SECOND;
    let seconds =
        i64::from(clock.hour) * 3600 + i64::from(clock.minute) * 60 + i64::from(clock.second)
            - offset_seconds;
    let micros = days_from_civil(year, month, day) * MICROS_PER_DAY
        + seconds * MICROS_PER_SECOND
        + clock.fraction.map_or(0, rounded_microseconds);
    let (year, month, day) = civil_from_days(micros.div_euclid(MICROS_PER_DAY));
    if !(1..=9999).contains(&year) {
        return Err(Error::forward_refusal(format!(
            "\"{text}\" falls outside the years 1 to 9999 once it is moved to UTC, and the \
             replica holds a timestamp with time zone as UTC text with a four-digit year."
        )));
    }
    let micros_of_day = micros.rem_euclid(MICROS_PER_DAY);
    let second_of_day = micros_of_day / MICROS_PER_SECOND;
    Ok(format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:06}+00:00",
        second_of_day / 3600,
        second_of_day / 60 % 60,
        second_of_day % 60,
        micros_of_day % MICROS_PER_SECOND
    ))
}

/// The fraction of a second in microseconds, rounded as PostgreSQL rounds it,
/// which is `rint(strtod(fraction) * 1e6)`.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "the scaled fraction lies in [0, 1e6], exact in both f64 and i64"
)]
fn rounded_microseconds(fraction: &str) -> i64 {
    let scaled = format!("0.{fraction}").parse::<f64>().unwrap_or_default() * 1e6;
    debug_assert!((0.0..=1e6).contains(&scaled), "{fraction} is not a fraction of a second");
    let whole = scaled as i64; // truncation toward zero, the rounding follows
    match (scaled - whole as f64).partial_cmp(&0.5) {
        Some(core::cmp::Ordering::Greater) => whole + 1,
        Some(core::cmp::Ordering::Equal) => whole + (whole & 1),
        _ => whole,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: u32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The proleptic Gregorian date of a day count since 1970-01-01, with a
/// signed year so that a date before year 1 can be refused.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 { month_from_march + 3 } else { month_from_march - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    debug_assert!((1..=31).contains(&day) && (1..=12).contains(&month), "civil arithmetic");
    (year, u32::try_from(month).unwrap_or(1), u32::try_from(day).unwrap_or(1))
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

/// A UTC offset as written, kept apart from its text so a timestamp can be
/// moved to UTC and a time can print it as `±HH:MM`.
#[derive(Clone, Copy)]
struct UtcOffset {
    negative: bool,
    hours: u32,
    minutes: u32,
}

impl UtcOffset {
    const UTC: Self = Self { negative: false, hours: 0, minutes: 0 };

    /// `±HH:MM`, the only offset form SQLite's date functions read.
    fn render(self) -> String {
        let sign = if self.negative { '-' } else { '+' };
        format!("{sign}{:02}:{:02}", self.hours, self.minutes)
    }

    fn seconds(self) -> i64 {
        let magnitude = i64::from(self.hours) * 3600 + i64::from(self.minutes) * 60;
        if self.negative { -magnitude } else { magnitude }
    }
}

/// Splits a UTC offset off the end.
///
/// The search starts after the time part, since a date's own separators are
/// hyphens and `'2024-01-01'` would otherwise read `-01` as an offset.
fn split_time_zone(
    kind: TemporalLiteralKind,
    text: &str,
    zoned: bool,
) -> Result<(&str, Option<UtcOffset>), Error> {
    let time_start = match kind {
        TemporalLiteralKind::Time { .. } => Some(0),
        _ => text.find([' ', 'T', 't']).map(|index| index + 1),
    };
    let offset_at =
        time_start.and_then(|start| text[start..].find(['+', '-']).map(|index| index + start));
    let (body, zone) = match offset_at {
        Some(index) => (&text[..index], Some(&text[index..])),
        None => {
            match text.strip_suffix(['Z', 'z']) {
                Some(body) => (body, Some("Z")),
                None => (text, None),
            }
        }
    };
    let Some(zone) = zone else { return Ok((text, None)) };
    if !zoned {
        return Err(unreadable(
            kind,
            text,
            "this column has no time zone, so an offset in the value would be dropped.",
        ));
    }
    let offset = if zone == "Z" { UtcOffset::UTC } else { parse_offset(kind, zone, text)? };
    Ok((body.trim_end(), Some(offset)))
}

/// Reads `±HH[:MM]`, whose sign is its first character.
fn parse_offset(kind: TemporalLiteralKind, zone: &str, text: &str) -> Result<UtcOffset, Error> {
    let (sign, digits) = zone.split_at(1);
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
    Ok(UtcOffset { negative: sign == "-", hours, minutes })
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

/// A validated time of day, with the fraction of a second as written.
struct Clock<'a> {
    hour: u32,
    minute: u32,
    second: u32,
    fraction: Option<&'a str>,
}

impl Clock<'_> {
    const MIDNIGHT: Clock<'static> = Clock { hour: 0, minute: 0, second: 0, fraction: None };

    /// The text PostgreSQL prints, which keeps the fraction's digits as given.
    fn render(&self) -> String {
        let Self { hour, minute, second, fraction } = self;
        match fraction {
            Some(fraction) => format!("{hour:02}:{minute:02}:{second:02}.{fraction}"),
            None => format!("{hour:02}:{minute:02}:{second:02}"),
        }
    }
}

/// Parses `HH:MM[:SS[.frac]]` with unpadded components allowed.
fn parse_time<'a>(
    kind: TemporalLiteralKind,
    time: &'a str,
    text: &str,
) -> Result<Clock<'a>, Error> {
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
    Ok(Clock { hour, minute, second, fraction })
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
///
/// `month` is validated before it reaches here, and an out-of-range one is
/// clamped rather than given an arm of its own, so no branch exists that a
/// test could not reach.
fn days_in_month(year: u32, month: u32) -> u32 {
    const LENGTHS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if month == 2
        && year.is_multiple_of(4)
        && (!year.is_multiple_of(100) || year.is_multiple_of(400))
    {
        return 29;
    }
    LENGTHS[(month.clamp(1, 12) - 1) as usize]
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
