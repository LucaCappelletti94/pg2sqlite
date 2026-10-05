//! F1: date and timestamp arithmetic that carries no INTERVAL.
//!
//! Without an INTERVAL operand PostgreSQL still has a whole operator family
//! over `date`, `timestamp` and `time`, and SQLite holds all three as text.
//! Left alone, `date '2026-08-07' - date '2026-08-01'` reaches SQLite as text
//! minus text and answers 0. Every expectation below was read off PostgreSQL
//! 17 and is asserted by running the translator's own output.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

const DDL: &str = "CREATE TABLE dt (id INT PRIMARY KEY, d1 DATE, d2 DATE, ts1 TIMESTAMP, \
                   ts2 TIMESTAMP, tm1 TIME, tm2 TIME, n INT, s TEXT);";

/// Row 1 carries values, row 2 is all NULL so NULL propagation is observable.
const ROWS: &str = "INSERT INTO dt (id, d1, d2, ts1, ts2, tm1, tm2, n, s) VALUES \
                    (1, '2026-08-01', '2026-08-07', '2026-08-01 10:00:00', \
                    '2026-08-07 12:00:00', '12:00:00', '14:00:00', 7, 'x');
                    INSERT INTO dt (id) VALUES (2);";

/// Translate and run `SELECT <select_list> FROM dt WHERE id = <id>`, returning
/// the single value it projects.
fn eval(select_list: &str, id: u8) -> Option<String> {
    let pg = format!("{DDL}\n{ROWS}\nSELECT {select_list} FROM dt WHERE id = {id};");
    run_translated_with(&pg, &Pg2SqliteOptions::default())
        .into_iter()
        .next()
        .expect("the probe should project one row")
}

/// The refusal message for a select list the translator must not emit.
fn refusal(select_list: &str) -> String {
    let pg = format!("{DDL}\nSELECT {select_list} FROM dt;");
    Pg2Sqlite::default()
        .sql(&pg)
        .expect("parses")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err(&format!("expected a refusal for: {select_list}"))
        .to_string()
}

// ── date minus date, which PostgreSQL answers in whole days
// ──────────────────

/// PostgreSQL `date '2026-08-07' - date '2026-08-01'` = 6.
#[test]
fn date_minus_date_counts_whole_days() {
    assert_eq!(eval("d2 - d1", 1).as_deref(), Some("6"));
}

/// The same over literals rather than columns.
#[test]
fn date_minus_date_counts_whole_days_over_literals() {
    assert_eq!(eval("date '2026-08-07' - date '2026-08-01'", 1).as_deref(), Some("6"));
}

/// PostgreSQL `d1 - d2` = -6 when d1 is the earlier date.
#[test]
fn date_minus_date_is_negative_when_reversed() {
    assert_eq!(eval("d1 - d2", 1).as_deref(), Some("-6"));
}

/// PostgreSQL answers NULL when either date is NULL.
#[test]
fn date_minus_date_propagates_null() {
    assert_eq!(eval("d2 - d1", 2), None);
}

// ── date plus or minus a whole number of days
// ────────────────────────────────

/// PostgreSQL `date '2026-08-01' + 7` = 2026-08-08.
#[test]
fn date_plus_integer_moves_forward() {
    assert_eq!(eval("d1 + 7", 1).as_deref(), Some("2026-08-08"));
}

/// The operator commutes in PostgreSQL: `7 + date '2026-08-01'` = 2026-08-08.
#[test]
fn integer_plus_date_moves_forward() {
    assert_eq!(eval("7 + d1", 1).as_deref(), Some("2026-08-08"));
}

/// PostgreSQL `date '2026-08-01' - 7` = 2026-07-25.
#[test]
fn date_minus_integer_moves_back() {
    assert_eq!(eval("d1 - 7", 1).as_deref(), Some("2026-07-25"));
}

/// The day count may be a column, not just a literal. PostgreSQL answers
/// 2026-08-08 for n = 7.
#[test]
fn date_plus_integer_column_moves_forward() {
    assert_eq!(eval("d1 + n", 1).as_deref(), Some("2026-08-08"));
}

/// PostgreSQL answers NULL for `date + NULL::int`. This is what rules out a
/// `printf('%+d days', n)` modifier, which formats NULL as `+0 days` and would
/// answer the unchanged date.
#[test]
fn date_plus_null_integer_is_null() {
    assert_eq!(eval("d1 + n", 2), None);
}

/// A cast names the type just as a column declaration does. PostgreSQL
/// `CAST('2026-08-01' AS DATE) + 1` = 2026-08-02.
#[test]
fn cast_to_date_plus_integer_moves_forward() {
    assert_eq!(eval("CAST('2026-08-01' AS DATE) + 1", 1).as_deref(), Some("2026-08-02"));
}

/// A rewritten subexpression is itself a date, so the family composes.
/// PostgreSQL `(d1 + 7) - d1` = 7.
#[test]
fn rewritten_date_composes_with_further_arithmetic() {
    assert_eq!(eval("(d1 + 7) - d1", 1).as_deref(), Some("7"));
}

/// `CURRENT_DATE` is a date without a declaration to read it from.
/// PostgreSQL `(CURRENT_DATE + 1) - CURRENT_DATE` = 1, whatever the day.
#[test]
fn current_date_is_recognised_as_a_date() {
    assert_eq!(eval("(CURRENT_DATE + 1) - CURRENT_DATE", 1).as_deref(), Some("1"));
}

// ── the shapes PostgreSQL answers with an interval
// ───────────────────────────

/// `timestamp - timestamp` is an interval in PostgreSQL and SQLite has no
/// interval, so it is refused rather than emitted as text arithmetic.
#[test]
fn timestamp_difference_is_refused() {
    let message = refusal("ts2 - ts1");
    assert!(message.contains("interval"), "the message should name the interval result: {message}");
    assert!(
        message.contains("extract(epoch"),
        "the message should name the form that does translate: {message}"
    );
}

/// `time - time` is an interval too.
#[test]
fn time_difference_is_refused() {
    let message = refusal("tm2 - tm1");
    assert!(message.contains("interval"), "the message should name the interval result: {message}");
}

/// PostgreSQL widens the date to a timestamp and answers an interval.
#[test]
fn date_minus_timestamp_is_refused() {
    let message = refusal("d1 - ts1");
    assert!(message.contains("interval"), "the message should name the interval result: {message}");
}

/// `date + time` is a timestamp in PostgreSQL and has no SQLite operator.
#[test]
fn date_plus_time_is_refused() {
    let message = refusal("d1 + tm1");
    assert!(
        message.contains("timestamp"),
        "the message should name the timestamp result: {message}"
    );
}

/// PostgreSQL would accept an integer, a time or an interval on the right of a
/// date, and the three translate differently, so an operand whose type cannot
/// be resolved is refused rather than guessed.
#[test]
fn date_plus_an_unresolvable_operand_is_refused() {
    let message = refusal("d1 + (SELECT 1)");
    assert!(
        message.contains("whole number of days"),
        "the message should say what is missing: {message}"
    );
}

// ── the interval, erased by extract(epoch from ...)
// ──────────────────────────

/// PostgreSQL `extract(epoch from (ts2 - ts1))` = 525600 seconds.
#[test]
fn epoch_of_a_timestamp_difference_is_seconds() {
    assert_eq!(eval("extract(epoch from (ts2 - ts1))", 1).as_deref(), Some("525600"));
}

/// `date_part('epoch', ...)` is the same operation spelled as a function.
#[test]
fn date_part_epoch_of_a_timestamp_difference_is_seconds() {
    assert_eq!(eval("date_part('epoch', ts2 - ts1)", 1).as_deref(), Some("525600"));
}

/// PostgreSQL answers a numeric, so dividing the seconds keeps the fraction:
/// `extract(epoch from (ts2 - ts1)) / 1000` = 525.6. SQLite would answer 525
/// if the difference came back as an integer, which is why the `'subsec'`
/// modifier is load-bearing rather than decoration.
#[test]
fn epoch_of_a_timestamp_difference_divides_as_a_number() {
    assert_eq!(eval("extract(epoch from (ts2 - ts1)) / 1000", 1).as_deref(), Some("525.6"));
}

/// PostgreSQL keeps the fraction: the same span with a `.25` second operand is
/// 525600.25. This is what rules out `(julianday(a) - julianday(b)) * 86400.0`,
/// which answers 525600.000013411 for the whole-second span.
#[test]
fn epoch_of_a_timestamp_difference_keeps_subseconds() {
    let select_list = "extract(epoch from \
                       (timestamp '2026-08-07 12:00:00.25' - timestamp '2026-08-01 10:00:00'))";
    assert_eq!(eval(select_list, 1).as_deref(), Some("525600.25"));
}

/// PostgreSQL `extract(epoch from (time '14:00' - time '12:00'))` = 7200.
#[test]
fn epoch_of_a_time_difference_is_seconds() {
    assert_eq!(eval("extract(epoch from (tm2 - tm1))", 1).as_deref(), Some("7200"));
}

// ── everything outside the family is untouched
// ───────────────────────────────

/// Arithmetic over two integers keeps its ordinary translation.
#[test]
fn integer_arithmetic_is_untouched() {
    assert_eq!(eval("n + n", 1).as_deref(), Some("14"));
}

/// The INTERVAL interception still owns its own shape.
#[test]
fn interval_arithmetic_is_untouched() {
    assert_eq!(eval("ts1 + interval '1 day'", 1).as_deref(), Some("2026-08-02 10:00:00"));
}

// ── the crate reads its own output back
// ──────────────────────────────────────

/// Forward-translate `pg`, then reverse the emitted statement, and return the
/// PostgreSQL it comes back as.
fn round_trip(select_list: &str) -> String {
    let options = Pg2SqliteOptions::default();
    let translator =
        Pg2Sqlite::default().sql(&format!("{DDL}\nSELECT {select_list} FROM dt;")).expect("parses");
    let schema = translator.build_schema().expect("schema builds");
    let emitted =
        translator.clone().translate_to_sql(&options).expect("translates").pop().expect("a query");
    let reversed = translator
        .reverse_sql(&format!("{emitted};"), &schema, &options)
        .unwrap_or_else(|error| panic!("reversing {emitted} failed: {error}"));
    reversed[0].to_string()
}

/// `julianday` has no PostgreSQL name, so the day count reverses as a whole
/// shape or not at all.
#[test]
fn a_day_count_reverses_to_date_subtraction() {
    assert_eq!(round_trip("d2 - d1"), "SELECT d2 - d1 FROM dt");
}

/// The same for a date moved by whole days, in both directions.
#[test]
fn a_shifted_date_reverses_to_date_arithmetic() {
    assert_eq!(round_trip("d1 + 7"), "SELECT d1 + 7 FROM dt");
    assert_eq!(round_trip("d1 - n"), "SELECT d1 - n FROM dt");
}

/// `unixepoch(x, 'subsec')` has no PostgreSQL name either, and the pair of
/// them is one EXTRACT.
#[test]
fn an_epoch_difference_reverses_to_extract() {
    assert_eq!(
        round_trip("extract(epoch from (ts2 - ts1))"),
        "SELECT EXTRACT(EPOCH FROM (ts2 - ts1)) FROM dt"
    );
}
