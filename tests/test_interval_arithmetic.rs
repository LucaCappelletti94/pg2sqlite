//! F3: `timestamp + interval` must land where PostgreSQL lands.
//!
//! PostgreSQL holds an interval as three independent counts, months, days and
//! microseconds, and applies them in that order, clamping an overflowed
//! day-of-month to the end of the target month. SQLite has a date modifier per
//! unit, applies them left to right, rolls an overflowed day forward instead
//! of clamping, and knows only six unit words.
//!
//! Every expected value below was read off PostgreSQL 17 before the fix, and
//! every case runs the crate's own emitted SQL, because what this guards is a
//! date, not a keyword.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use run_translated_helper::run_translated_with;

/// The instant `expression` evaluates to, through the emitted SQLite.
fn shift(expression: &str) -> String {
    let script = format!("SELECT {expression};");
    run_translated_with(&script, &Pg2SqliteOptions::default())
        .remove(0)
        .unwrap_or_else(|| panic!("{expression} evaluated to NULL"))
}

fn refusal(expression: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("SELECT {expression};"))
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("this shape has no SQLite form")
        .to_string()
}

// ---------------------------------------------------------------------------
// The month-end clamp
// ---------------------------------------------------------------------------

/// The item's own trigger. SQLite rolls 31 February forward to 3 March,
/// PostgreSQL clamps it to the last day of February.
#[test]
fn adding_a_month_clamps_to_the_month_end() {
    assert_eq!(shift("timestamp '2026-01-31' + interval '1 month'"), "2026-02-28 00:00:00");
}

/// The clamp is not specific to addition.
#[test]
fn subtracting_a_month_clamps_to_the_month_end() {
    assert_eq!(shift("timestamp '2026-03-31' - interval '1 month'"), "2026-02-28 00:00:00");
}

/// A leap day one year on has nowhere to land either.
#[test]
fn adding_a_year_clamps_a_leap_day() {
    assert_eq!(shift("timestamp '2024-02-29' + interval '1 year'"), "2025-02-28 00:00:00");
}

/// A day that exists in the target month is left where it is, which is what
/// makes the clamp safe to apply unconditionally.
#[test]
fn a_day_that_exists_is_untouched() {
    assert_eq!(shift("timestamp '2026-01-15' + interval '1 month'"), "2026-02-15 00:00:00");
    assert_eq!(shift("timestamp '2026-01-31' + interval '2 months'"), "2026-03-31 00:00:00");
}

/// An interval with no month component cannot overflow a month, so it must not
/// pick up a clamp that would change nothing but would still have to be right.
#[test]
fn days_alone_roll_over_the_month() {
    assert_eq!(shift("timestamp '2026-01-31' + interval '30 days'"), "2026-03-02 00:00:00");
}

// ---------------------------------------------------------------------------
// The three counts, and the order they are applied in
// ---------------------------------------------------------------------------

/// PostgreSQL adds thirteen months and clamps once. Clamping a year and then
/// a month lands a day earlier, on 2025-03-28, which is what a per-unit
/// modifier chain produces however the clamp is placed.
#[test]
fn a_year_and_a_month_clamp_once_together() {
    assert_eq!(shift("timestamp '2024-02-29' + interval '1 year 1 month'"), "2025-03-29 00:00:00");
}

/// The months go on before the days no matter which order they were written
/// in, so both spellings give the same instant.
#[test]
fn months_are_applied_before_days() {
    assert_eq!(shift("timestamp '2026-01-30' + interval '1 day 1 month'"), "2026-03-01 00:00:00");
    assert_eq!(shift("timestamp '2026-01-30' + interval '1 month 1 day'"), "2026-03-01 00:00:00");
}

/// Clamp first, then add the day: 28 February plus one day, not 3 March plus
/// one day.
#[test]
fn the_clamp_lands_before_the_days_are_added() {
    assert_eq!(shift("timestamp '2026-01-31' + interval '1 month 1 day'"), "2026-03-01 00:00:00");
}

/// A field can carry its own sign, and the negative day still follows the
/// clamped month.
#[test]
fn a_field_keeps_its_own_sign() {
    assert_eq!(shift("timestamp '2026-01-31' + interval '1 month -1 day'"), "2026-02-27 00:00:00");
}

/// Subtraction negates every field, so the day becomes an addition.
#[test]
fn subtraction_negates_every_field() {
    assert_eq!(shift("timestamp '2026-03-31' - interval '1 month -1 day'"), "2026-03-01 00:00:00");
}

/// All three counts at once.
#[test]
fn months_days_and_time_together() {
    assert_eq!(
        shift("timestamp '2026-01-31 08:00:00' + interval '1 year 2 mons 3 days 4 hours'"),
        "2027-04-03 12:00:00"
    );
}

// ---------------------------------------------------------------------------
// Unit names SQLite does not know
// ---------------------------------------------------------------------------

/// SQLite's date modifier has no week, so `'+1 week'` made `datetime()`
/// return NULL with no error at all.
#[test]
fn a_week_is_seven_days() {
    assert_eq!(shift("timestamp '2026-01-31' + interval '1 week'"), "2026-02-07 00:00:00");
}

/// Every abbreviation PostgreSQL accepts and SQLite does not.
#[test]
fn abbreviated_units_are_translated() {
    assert_eq!(shift("timestamp '2026-01-31' + interval '1 mon'"), "2026-02-28 00:00:00");
    assert_eq!(shift("timestamp '2026-01-31' + interval '1 yr'"), "2027-01-31 00:00:00");
    assert_eq!(shift("timestamp '2026-01-31' + interval '1 hr'"), "2026-01-31 01:00:00");
    assert_eq!(shift("timestamp '2026-01-31' + interval '1 min'"), "2026-01-31 00:01:00");
    assert_eq!(shift("timestamp '2026-01-31' + interval '1 sec'"), "2026-01-31 00:00:01");
}

/// The units above a year are all a count of months.
#[test]
fn the_long_units_are_months() {
    assert_eq!(shift("timestamp '2026-01-31' + interval '1 decade'"), "2036-01-31 00:00:00");
    assert_eq!(shift("timestamp '2026-01-31' + interval '1 century'"), "2126-01-31 00:00:00");
    assert_eq!(shift("timestamp '2026-01-31' + interval '1 millennium'"), "3026-01-31 00:00:00");
}

/// A unit neither engine knows is refused by name rather than emitted into a
/// `datetime()` call that answers NULL.
#[test]
fn an_unknown_unit_is_refused() {
    let error = refusal("timestamp '2026-01-31' + interval '1 fortnight'");
    assert!(error.contains("fortnight"), "the refusal must name the unit: {error}");
}

// ---------------------------------------------------------------------------
// Fractional counts
// ---------------------------------------------------------------------------

/// A fraction of a unit above a month is rounded to whole months: one and a
/// half years is eighteen months, with no stray days.
#[test]
fn a_fractional_year_becomes_whole_months() {
    assert_eq!(shift("timestamp '2026-01-31' + interval '1.5 years'"), "2027-07-31 00:00:00");
}

/// A fraction of a month spills into days at thirty days to the month, so
/// one and seven tenths months is one month and twenty one days.
#[test]
fn a_fractional_month_spills_into_days() {
    assert_eq!(shift("timestamp '2026-01-31' + interval '1.7 months'"), "2026-03-21 00:00:00");
}

/// A fraction of a day spills into the time of day.
#[test]
fn a_fractional_day_spills_into_the_time() {
    assert_eq!(
        shift("timestamp '2026-01-31 12:00:00' + interval '1.5 days'"),
        "2026-02-02 00:00:00"
    );
    assert_eq!(shift("timestamp '2026-01-31' + interval '1.5 weeks'"), "2026-02-10 12:00:00");
}

// ---------------------------------------------------------------------------
// Other spellings of the same thing
// ---------------------------------------------------------------------------

/// `INTERVAL '1' MONTH` puts the unit in its own clause, and reaches the same
/// arithmetic.
#[test]
fn the_leading_field_spelling_clamps_too() {
    assert_eq!(shift("timestamp '2026-01-31' + interval '1' month"), "2026-02-28 00:00:00");
    assert_eq!(shift("timestamp '2026-01-31' + interval '1' week"), "2026-02-07 00:00:00");
}

/// `INTERVAL '1-2' YEAR TO MONTH` is a range spelling this does not decode,
/// and it used to emit `'+1-2 year'`, which answers NULL.
#[test]
fn the_range_spelling_is_refused() {
    let error = refusal("timestamp '2026-01-31' + interval '1-2' year to month");
    assert!(error.contains("INTERVAL"), "the refusal must name the construct: {error}");
}

/// Two spellings of the same duration collapse onto one modifier, and it is
/// the largest unit that divides the total exactly.
#[test]
fn the_time_of_day_is_emitted_in_one_unit() {
    assert_eq!(
        shift("timestamp '2026-01-31' + interval '1 hour 30 minutes'"),
        "2026-01-31 01:30:00"
    );
    let emitted = Pg2Sqlite::default()
        .sql("SELECT timestamp '2026-01-31' + interval '1 hour 30 minutes';")
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .unwrap()
        .join("\n");
    assert!(emitted.contains("'+90 minutes'"), "one exact modifier, got: {emitted}");
}
