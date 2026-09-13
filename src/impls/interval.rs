//! PostgreSQL `INTERVAL` operands, lowered onto SQLite date modifiers.
//!
//! PostgreSQL does not hold an interval as the units it was written in. It
//! holds three independent counts, months, days and microseconds, and when it
//! adds one to a timestamp it applies them in that order, clamping a
//! day-of-month that the target month does not have to that month's last day.
//! SQLite has one modifier per unit, applies them left to right, rolls an
//! overflowed day forward instead of clamping, and its date modifier knows
//! only `days hours minutes seconds months years`.
//!
//! Emitting one modifier per written unit therefore diverged three ways, all
//! measured against PostgreSQL 17: `'2026-01-31' + interval '1 month'`
//! answered 3 March rather than 28 February, `'2024-02-29' + interval '1 year
//! 1 month'` clamped twice and answered a day early, and `interval '1 week'`
//! made `datetime()` answer NULL with no error anywhere.
//!
//! So this reproduces PostgreSQL's own decomposition and emits it in
//! PostgreSQL's own order: `'+M months', 'floor', '+D days', '+S seconds'`,
//! where `floor` is SQLite's month-end clamp (3.46.0, exactly the declared
//! floor). Every count is exact integer arithmetic over the written decimal,
//! not floating point, because a float turns `interval '1.7 months'` into 20
//! days and a microsecond short of one more where PostgreSQL says 21 days.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use sqlparser::ast::Interval;

use crate::{errors::Error, impls::function_helpers::single_quoted_literal};

/// Microseconds in a day. PostgreSQL keeps days apart from microseconds
/// because a day is not always 24 hours in a zone that observes daylight
/// saving, but SQLite has no zones and its own modifiers make the two
/// interchangeable, so folding them here costs nothing and the emission
/// splits them apart again.
const MICROS_PER_DAY: i128 = 86_400_000_000;
const MICROS_PER_HOUR: i128 = 3_600_000_000;
const MICROS_PER_MINUTE: i128 = 60_000_000;
const MICROS_PER_SECOND: i128 = 1_000_000;

/// Days PostgreSQL gives a month when a fraction of one spills downwards.
const DAYS_PER_FRACTIONAL_MONTH: i128 = 30;

/// What one of a unit counts.
#[derive(Clone, Copy)]
enum UnitScale {
    /// The word `month` itself, whose fraction spills into days.
    Month,
    /// A unit above a month, whose fraction PostgreSQL rounds to whole
    /// months instead of spilling: `interval '1.4 years'` is 17 months, not
    /// 16 months and 24 days.
    Months(i128),
    /// Everything at a day or below, in microseconds.
    Micros(i128),
}

/// The unit words PostgreSQL 17 accepts, read out of the engine rather than
/// from the documentation. `cents` is deliberately absent: PostgreSQL rejects
/// it while accepting `cent` and `centuries`.
fn unit_scale(word: &str) -> Option<UnitScale> {
    Some(match word {
        "microsecond" | "microseconds" | "us" | "usec" | "usecs" | "useconds" => {
            UnitScale::Micros(1)
        }
        "millisecond" | "milliseconds" | "ms" | "msec" | "msecs" | "mseconds" => {
            UnitScale::Micros(1_000)
        }
        "second" | "seconds" | "sec" | "secs" | "s" => UnitScale::Micros(MICROS_PER_SECOND),
        "minute" | "minutes" | "min" | "mins" | "m" => UnitScale::Micros(MICROS_PER_MINUTE),
        "hour" | "hours" | "hr" | "hrs" | "h" => UnitScale::Micros(MICROS_PER_HOUR),
        "day" | "days" | "d" => UnitScale::Micros(MICROS_PER_DAY),
        "week" | "weeks" | "w" => UnitScale::Micros(7 * MICROS_PER_DAY),
        "month" | "months" | "mon" | "mons" => UnitScale::Month,
        "year" | "years" | "yr" | "yrs" | "y" => UnitScale::Months(12),
        "decade" | "decades" | "dec" | "decs" => UnitScale::Months(120),
        "century" | "centuries" | "cent" => UnitScale::Months(1_200),
        "millennium" | "millennia" | "mil" | "mils" => UnitScale::Months(12_000),
        _ => return None,
    })
}

/// A written count, kept as an exact integer over a power of ten.
struct Decimal {
    numerator: i128,
    denominator: i128,
}

impl Decimal {
    /// Parses `+1`, `-2.5`, `.5` and `3.` and nothing else. PostgreSQL
    /// rejects an exponent here too (`interval '1e2 days'` is a syntax
    /// error), so refusing one loses nothing.
    fn parse(text: &str) -> Option<Self> {
        let (negative, digits) = match text.as_bytes().first()? {
            b'-' => (true, &text[1..]),
            b'+' => (false, &text[1..]),
            _ => (false, text),
        };
        let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
        if whole.is_empty() && fraction.is_empty() {
            return None;
        }
        if !whole.bytes().chain(fraction.bytes()).all(|byte| byte.is_ascii_digit()) {
            return None;
        }

        let denominator = 10_i128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
        let mut numerator: i128 = 0;
        for byte in whole.bytes().chain(fraction.bytes()) {
            numerator = numerator.checked_mul(10)?.checked_add(i128::from(byte - b'0'))?;
        }
        Some(Self { numerator: if negative { -numerator } else { numerator }, denominator })
    }
}

/// `numerator / denominator`, truncated towards zero, which is what
/// PostgreSQL does with a fraction it cannot represent: `interval '0.0000015
/// seconds'` is one microsecond, not two.
fn truncating_div(numerator: i128, denominator: i128) -> i128 {
    numerator / denominator
}

/// `numerator / denominator`, rounded half away from zero, which is what
/// PostgreSQL does to a fraction of a unit above a month: 13.5 months is 14.
fn rounding_div(numerator: i128, denominator: i128) -> i128 {
    let half = denominator / 2;
    if numerator >= 0 { (numerator + half) / denominator } else { (numerator - half) / denominator }
}

/// The three counts a PostgreSQL interval holds, with days folded into
/// microseconds.
#[derive(Default)]
struct IntervalFields {
    months: i128,
    micros: i128,
}

impl IntervalFields {
    fn add(&mut self, count: &Decimal, scale: UnitScale) -> Option<()> {
        match scale {
            UnitScale::Months(per_unit) => {
                self.months = self.months.checked_add(rounding_div(
                    count.numerator.checked_mul(per_unit)?,
                    count.denominator,
                ))?;
            }
            UnitScale::Month => {
                let whole = truncating_div(count.numerator, count.denominator);
                self.months = self.months.checked_add(whole)?;
                let remainder =
                    count.numerator.checked_sub(whole.checked_mul(count.denominator)?)?;
                self.micros = self.micros.checked_add(truncating_div(
                    remainder.checked_mul(DAYS_PER_FRACTIONAL_MONTH * MICROS_PER_DAY)?,
                    count.denominator,
                ))?;
            }
            UnitScale::Micros(per_unit) => {
                self.micros = self.micros.checked_add(truncating_div(
                    count.numerator.checked_mul(per_unit)?,
                    count.denominator,
                ))?;
            }
        }
        Some(())
    }

    fn negate(&mut self) {
        self.months = -self.months;
        self.micros = -self.micros;
    }

    /// Multiply both counts by `factor`, returning `None` on overflow.
    fn scale(&mut self, factor: i128) -> Option<()> {
        self.months = self.months.checked_mul(factor)?;
        self.micros = self.micros.checked_mul(factor)?;
        Some(())
    }

    /// The modifiers, in PostgreSQL's order of operations.
    fn modifiers(&self) -> Vec<String> {
        let days = self.micros / MICROS_PER_DAY;
        let time = self.micros % MICROS_PER_DAY;

        let mut modifiers = Vec::new();
        if self.months != 0 {
            modifiers.push(signed(self.months, "months"));
            // Clamps the day of month the months step overflowed, which is
            // what PostgreSQL does and what SQLite does not. It has to sit
            // here rather than at the end: the days below are added to the
            // clamped date, not clamped along with it.
            modifiers.push("floor".into());
        }
        if days != 0 {
            modifiers.push(signed(days, "days"));
        }
        if time != 0 {
            modifiers.push(time_modifier(time));
        }
        modifiers
    }
}

/// `+3 days`, `-1 months`.
fn signed(count: i128, unit: &str) -> String {
    let sign = if count < 0 { '-' } else { '+' };
    format!("{sign}{} {unit}", count.unsigned_abs())
}

/// The time of day as one modifier, in the largest unit that divides it
/// exactly, so `interval '1 hour 30 minutes'` stays readable as `+90
/// minutes` rather than becoming a second count.
fn time_modifier(micros: i128) -> String {
    if micros % MICROS_PER_HOUR == 0 {
        return signed(micros / MICROS_PER_HOUR, "hours");
    }
    if micros % MICROS_PER_MINUTE == 0 {
        return signed(micros / MICROS_PER_MINUTE, "minutes");
    }
    if micros % MICROS_PER_SECOND == 0 {
        return signed(micros / MICROS_PER_SECOND, "seconds");
    }
    let sign = if micros < 0 { '-' } else { '+' };
    let magnitude = micros.unsigned_abs();
    let fraction = format!("{:06}", magnitude % MICROS_PER_SECOND.unsigned_abs());
    format!(
        "{sign}{}.{} seconds",
        magnitude / MICROS_PER_SECOND.unsigned_abs(),
        fraction.trim_end_matches('0')
    )
}

/// The `(count, unit)` pairs an interval body spells out, or `None` when the
/// shape is one this does not decode.
fn unit_pairs(interval: &Interval) -> Option<Vec<(&str, String)>> {
    // `INTERVAL '1-2' YEAR TO MONTH` packs two fields into one string in a
    // notation of its own.
    if interval.last_field.is_some() {
        return None;
    }
    let body = single_quoted_literal(interval.value.as_ref())?;
    let tokens: Vec<&str> = body.split_whitespace().collect();

    // `INTERVAL '1' MONTH` puts the unit in a clause of its own.
    if let Some(field) = &interval.leading_field {
        let [count] = tokens[..] else { return None };
        return Some(vec![(count, field.to_string().to_lowercase())]);
    }

    if tokens.is_empty() || !tokens.len().is_multiple_of(2) {
        return None;
    }
    Some(tokens.chunks(2).map(|pair| (pair[0], pair[1].to_lowercase())).collect())
}

/// The SQLite date modifiers for `interval`, negated for subtraction.
///
/// `Ok(None)` means the interval is spelled in a notation this does not
/// decode, so the caller falls through to the standalone-INTERVAL refusal.
/// `Err` means it was decoded and cannot be expressed, which is worth its own
/// message because the alternative is a `datetime()` call answering NULL.
pub(crate) fn interval_date_modifiers(
    interval: &Interval,
    negate: bool,
) -> Result<Option<Vec<String>>, Error> {
    // Scaling by one is the identity: `IntervalFields::scale` multiplies both
    // fields with `checked_mul`, which cannot overflow at a factor of one.
    interval_date_modifiers_scaled(interval, negate, 1)
}

/// The SQLite date modifiers for `interval * factor`, negated for subtraction.
///
/// The integer scalar from `n * INTERVAL 'x'` or `INTERVAL 'x' * n` is folded
/// into the counts before the modifiers are rendered. This is needed because
/// the modifier string format (`+3 days`) does not compose with post-rendering
/// multiplication.
///
/// `Ok(None)` means the interval notation is not recognised (caller emits a
/// notation-specific refusal). `Err` means decoded but overflows.
pub(crate) fn interval_date_modifiers_scaled(
    interval: &Interval,
    negate: bool,
    factor: i64,
) -> Result<Option<Vec<String>>, Error> {
    let Some(pairs) = unit_pairs(interval) else { return Ok(None) };

    let mut fields = IntervalFields::default();
    for (count, unit) in pairs {
        let Some(parsed) = Decimal::parse(count) else {
            return Err(Error::forward_refusal(format!(
                "INTERVAL '{count} {unit}' cannot be translated: {count} is not a plain decimal \
                 count. Write the interval as a sequence of count and unit pairs, such as \
                 INTERVAL '1 month 2 days'."
            )));
        };
        let Some(unit_s) = unit_scale(&unit) else {
            return Err(Error::forward_refusal(format!(
                "INTERVAL unit '{unit}' is not a PostgreSQL interval unit, so it has no SQLite \
                 date modifier. The units are microsecond, millisecond, second, minute, hour, \
                 day, week, month, year, decade, century and millennium, with their usual \
                 abbreviations."
            )));
        };
        if fields.add(&parsed, unit_s).is_none() {
            return Err(Error::forward_refusal(format!(
                "INTERVAL '{count} {unit}' is too large to translate: the count overflows the \
                 months and microseconds PostgreSQL would hold it in."
            )));
        }
    }

    if fields.scale(i128::from(factor)).is_none() {
        return Err(Error::forward_refusal(format!(
            "INTERVAL scaled by {factor} overflows the months and microseconds it would hold."
        )));
    }
    if negate {
        fields.negate();
    }
    Ok(Some(fields.modifiers()))
}

/// What one of a unit counts when a literal is being normalised, which keeps
/// days apart from the time of day.
///
/// PostgreSQL prints `1 day 02:03:04` for a written day and `26:00:00` for
/// twenty-six written hours, so the two cannot be folded together the way the
/// date modifiers fold them.
#[derive(Clone, Copy)]
enum LiteralScale {
    /// A unit above a month, whose fraction rounds to whole months.
    Months(i128),
    /// The word `month`, whose fraction spills downwards.
    Month,
    /// Days and weeks, whose fraction spills into the time of day.
    Days(i128),
    /// The time of day, in microseconds.
    Micros(i128),
}

/// The unit words a literal may carry, with days kept apart.
fn literal_unit_scale(word: &str) -> Option<LiteralScale> {
    Some(match word {
        "day" | "days" | "d" => LiteralScale::Days(1),
        "week" | "weeks" | "w" => LiteralScale::Days(7),
        other => {
            match unit_scale(other)? {
                UnitScale::Month => LiteralScale::Month,
                UnitScale::Months(per_unit) => LiteralScale::Months(per_unit),
                UnitScale::Micros(per_unit) => LiteralScale::Micros(per_unit),
            }
        }
    })
}

/// The three counts PostgreSQL holds an interval in, kept apart as it prints
/// them.
#[derive(Default)]
struct LiteralFields {
    months: i128,
    days: i128,
    micros: i128,
}

impl LiteralFields {
    fn add(&mut self, count: &Decimal, scale: LiteralScale) -> Option<()> {
        match scale {
            LiteralScale::Months(per_unit) => {
                self.months = self.months.checked_add(rounding_div(
                    count.numerator.checked_mul(per_unit)?,
                    count.denominator,
                ))?;
            }
            LiteralScale::Month => {
                let whole = truncating_div(count.numerator, count.denominator);
                self.months = self.months.checked_add(whole)?;
                let remainder =
                    count.numerator.checked_sub(whole.checked_mul(count.denominator)?)?;
                self.add_micros(truncating_div(
                    remainder.checked_mul(DAYS_PER_FRACTIONAL_MONTH * MICROS_PER_DAY)?,
                    count.denominator,
                ))?;
            }
            LiteralScale::Days(per_unit) => {
                let scaled = count.numerator.checked_mul(per_unit)?;
                let whole = truncating_div(scaled, count.denominator);
                self.days = self.days.checked_add(whole)?;
                let remainder = scaled.checked_sub(whole.checked_mul(count.denominator)?)?;
                self.micros = self.micros.checked_add(truncating_div(
                    remainder.checked_mul(MICROS_PER_DAY)?,
                    count.denominator,
                ))?;
            }
            LiteralScale::Micros(per_unit) => {
                self.micros = self.micros.checked_add(truncating_div(
                    count.numerator.checked_mul(per_unit)?,
                    count.denominator,
                ))?;
            }
        }
        Some(())
    }

    /// Adds microseconds, carrying whole days out of them, which is what a
    /// fraction of a month does: PostgreSQL answers `1 mon 15 days` for one
    /// and a half months rather than a count of hours.
    fn add_micros(&mut self, micros: i128) -> Option<()> {
        self.days = self.days.checked_add(micros / MICROS_PER_DAY)?;
        self.micros = self.micros.checked_add(micros % MICROS_PER_DAY)?;
        Some(())
    }

    fn negate(&mut self) {
        self.months = -self.months;
        self.days = -self.days;
        self.micros = -self.micros;
    }

    /// The text PostgreSQL prints for these counts under its default
    /// `IntervalStyle`.
    ///
    /// Each count carries its own sign, so `1 mon -1 day` prints as written,
    /// and a count is plural whenever it is not exactly one, which is why
    /// PostgreSQL answers `-1 days`. The time of day is printed only when it
    /// is non-zero, except for an interval that is zero throughout, which
    /// prints as a zero clock.
    fn to_postgres_text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let years = self.months / 12;
        let months = self.months % 12;
        if years != 0 {
            parts.push(format!("{years} {}", if years == 1 { "year" } else { "years" }));
        }
        if months != 0 {
            parts.push(format!("{months} {}", if months == 1 { "mon" } else { "mons" }));
        }
        if self.days != 0 {
            parts.push(format!("{} {}", self.days, if self.days == 1 { "day" } else { "days" }));
        }
        if self.micros != 0 || parts.is_empty() {
            parts.push(clock_text(self.micros));
        }
        parts.join(" ")
    }
}

/// The time of day as `[-]HH:MM:SS[.ffffff]`, with the hours unbounded.
fn clock_text(micros: i128) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let magnitude = micros.unsigned_abs();
    let seconds_total = magnitude / MICROS_PER_SECOND.unsigned_abs();
    let fraction = magnitude % MICROS_PER_SECOND.unsigned_abs();
    let clock = format!(
        "{sign}{:02}:{:02}:{:02}",
        seconds_total / 3_600,
        seconds_total % 3_600 / 60,
        seconds_total % 60
    );
    if fraction == 0 {
        return clock;
    }
    format!("{clock}.{}", format!("{fraction:06}").trim_end_matches('0'))
}

/// The text PostgreSQL prints for an interval literal, or the refusal it
/// answers for one it cannot read.
///
/// An interval column is stored as `TEXT`, so the text is the value: written
/// as `PT15M` it used to read back as `PT15M` where PostgreSQL answers
/// `00:15:00`, which made the replica and the server disagree about a value
/// neither of them had changed.
pub(crate) fn normalize_interval_literal(text: &str) -> Result<String, Error> {
    let body = text.trim().strip_prefix('@').unwrap_or(text.trim()).trim();
    let (body, negated) = match body.to_ascii_lowercase().strip_suffix("ago") {
        Some(head) if head.is_empty() || head.ends_with(' ') => (&body[..head.len()], true),
        _ => (body, false),
    };
    let mut fields = parse_interval_fields(body.trim(), text)?;
    if negated {
        fields.negate();
    }
    Ok(fields.to_postgres_text())
}

/// The counts an interval literal body spells out.
fn parse_interval_fields(body: &str, text: &str) -> Result<LiteralFields, Error> {
    if body.starts_with(['P', 'p']) {
        return parse_iso_interval(body, text);
    }
    if let Some(fields) = parse_year_month_interval(body) {
        return Ok(fields);
    }
    let mut fields = LiteralFields::default();
    let mut pending: Option<&str> = None;
    for token in body.split_whitespace() {
        if token.contains(':') {
            add_clock_token(&mut fields, pending.take(), token, text)?;
            continue;
        }
        let (count, unit) = split_count_and_unit(token);
        match unit {
            // `90 minutes`: the count is the token before this one.
            Some(unit) if count.is_empty() => {
                let count = pending
                    .take()
                    .ok_or_else(|| unreadable_interval(text, &format!("'{unit}' has no count")))?;
                add_unit(&mut fields, count, unit, text)?;
            }
            // `1day`, written as one token.
            Some(unit) => {
                if let Some(previous) = pending.take() {
                    add_unit(&mut fields, previous, "seconds", text)?;
                }
                add_unit(&mut fields, count, unit, text)?;
            }
            None => {
                if let Some(previous) = pending.replace(count) {
                    // Two counts in a row with no unit between them, which
                    // PostgreSQL does not read either.
                    return Err(unreadable_interval(text, &format!("'{previous}' names no unit")));
                }
            }
        }
    }
    if let Some(seconds) = pending {
        add_unit(&mut fields, seconds, "seconds", text)?;
    }
    Ok(fields)
}

/// Splits `1day` into its count and unit, answering `None` for a bare count.
fn split_count_and_unit(token: &str) -> (&str, Option<&str>) {
    match token.find(|c: char| c.is_ascii_alphabetic()) {
        Some(index) => (&token[..index], Some(&token[index..])),
        None => (token, None),
    }
}

/// Adds `count` of `unit` to the fields, refusing what PostgreSQL refuses.
fn add_unit(fields: &mut LiteralFields, count: &str, unit: &str, text: &str) -> Result<(), Error> {
    let Some(parsed) = Decimal::parse(count) else {
        return Err(unreadable_interval(text, &format!("'{count}' is not a number")));
    };
    let lowered = unit.to_ascii_lowercase();
    let Some(scale) = literal_unit_scale(&lowered) else {
        return Err(unreadable_interval(text, &format!("'{unit}' is not an interval unit")));
    };
    if fields.add(&parsed, scale).is_none() {
        return Err(unreadable_interval(text, "the counts overflow what PostgreSQL holds"));
    }
    Ok(())
}

/// Adds a `HH:MM[:SS[.f]]` token, with a bare count before it standing for
/// days: PostgreSQL reads `'1 2:03:04'` as one day and two hours.
fn add_clock_token(
    fields: &mut LiteralFields,
    leading_days: Option<&str>,
    token: &str,
    text: &str,
) -> Result<(), Error> {
    if let Some(days) = leading_days {
        add_unit(fields, days, "days", text)?;
    }
    let (negative, clock) = match token.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, token.strip_prefix('+').unwrap_or(token)),
    };
    let mut parts = clock.split(':');
    let hours = parts.next().unwrap_or_default();
    let minutes = parts.next().unwrap_or_default();
    let seconds = parts.next().unwrap_or("0");
    if parts.next().is_some() {
        return Err(unreadable_interval(text, "a clock reading has three parts at most"));
    }
    let mut clock_fields = LiteralFields::default();
    add_unit(&mut clock_fields, hours, "hours", text)?;
    add_unit(&mut clock_fields, minutes, "minutes", text)?;
    add_unit(&mut clock_fields, seconds, "seconds", text)?;
    if negative {
        clock_fields.negate();
    }
    // The hours stay hours: PostgreSQL answers `100:00:00` for a written
    // hundred hours rather than four days and four hours, because a written
    // clock reading never becomes a day count.
    let Some(micros) = fields.micros.checked_add(clock_fields.micros) else {
        return Err(unreadable_interval(text, "the counts overflow what PostgreSQL holds"));
    };
    fields.micros = micros;
    Ok(())
}

/// The SQL standard `'1-2'` form, one year and two months.
fn parse_year_month_interval(body: &str) -> Option<LiteralFields> {
    let (negative, digits) = match body.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, body.strip_prefix('+').unwrap_or(body)),
    };
    let (years, months) = digits.split_once('-')?;
    if years.is_empty()
        || months.is_empty()
        || !years.bytes().chain(months.bytes()).all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let total: i128 =
        years.parse::<i128>().ok()?.checked_mul(12)?.checked_add(months.parse().ok()?)?;
    Some(LiteralFields {
        months: if negative { -total } else { total },
        ..LiteralFields::default()
    })
}

/// The ISO 8601 `P1Y2M3DT4H5M6S` form, whose `M` means months before the `T`
/// and minutes after it.
fn parse_iso_interval(body: &str, text: &str) -> Result<LiteralFields, Error> {
    let mut fields = LiteralFields::default();
    let mut rest = &body[1..];
    let mut in_time = false;
    let mut count_start = 0;
    while count_start < rest.len() {
        if rest.as_bytes()[count_start] == b'T' || rest.as_bytes()[count_start] == b't' {
            in_time = true;
            rest = &rest[count_start + 1..];
            count_start = 0;
            continue;
        }
        let designator_at = rest[count_start..]
            .find(|c: char| c.is_ascii_alphabetic())
            .map(|index| index + count_start)
            .ok_or_else(|| unreadable_interval(text, "a count has no unit designator"))?;
        let count = &rest[count_start..designator_at];
        let unit = match (&rest[designator_at..=designator_at], in_time) {
            ("Y" | "y", _) => "years",
            ("M" | "m", false) => "months",
            ("W" | "w", _) => "weeks",
            ("D" | "d", _) => "days",
            ("H" | "h", true) => "hours",
            ("M" | "m", true) => "minutes",
            ("S" | "s", true) => "seconds",
            (other, _) => {
                return Err(unreadable_interval(
                    text,
                    &format!("'{other}' is not an ISO 8601 interval designator here"),
                ));
            }
        };
        add_unit(&mut fields, count, unit, text)?;
        count_start = designator_at + 1;
    }
    Ok(fields)
}

/// The refusal PostgreSQL answers for an interval it cannot read.
fn unreadable_interval(text: &str, reason: &str) -> Error {
    Error::forward_refusal(format!(
        "invalid input syntax for type interval: \"{text}\": {reason}. The replica stores an \
         interval column as the text PostgreSQL prints for it, so a value it cannot read is a \
         value the two databases would disagree about. Write it as a count and a unit, a clock \
         reading, or an ISO 8601 duration."
    ))
}
