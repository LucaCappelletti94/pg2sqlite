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
    let Some(pairs) = unit_pairs(interval) else { return Ok(None) };

    let mut fields = IntervalFields::default();
    for (count, unit) in pairs {
        let Some(parsed) = Decimal::parse(count) else {
            return Err(Error::UnsupportedSQLiteFeature(format!(
                "INTERVAL '{count} {unit}' cannot be translated: {count} is not a plain decimal \
                 count. Write the interval as a sequence of count and unit pairs, such as \
                 INTERVAL '1 month 2 days'."
            )));
        };
        let Some(scale) = unit_scale(&unit) else {
            return Err(Error::UnsupportedSQLiteFeature(format!(
                "INTERVAL unit '{unit}' is not a PostgreSQL interval unit, so it has no SQLite \
                 date modifier. The units are microsecond, millisecond, second, minute, hour, \
                 day, week, month, year, decade, century and millennium, with their usual \
                 abbreviations."
            )));
        };
        if fields.add(&parsed, scale).is_none() {
            return Err(Error::UnsupportedSQLiteFeature(format!(
                "INTERVAL '{count} {unit}' is too large to translate: the count overflows the \
                 months and microseconds PostgreSQL would hold it in."
            )));
        }
    }

    if negate {
        fields.negate();
    }
    Ok(Some(fields.modifiers()))
}
