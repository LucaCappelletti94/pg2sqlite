//! F33 and F34: the parts of a `to_char` template the lowering read wrongly.
//!
//! Two defects, one function. SQLite has no `%y`, so the two-digit year turned
//! the whole call into NULL. And PostgreSQL reads a template `T` as the start
//! of `TH` or `TM`, so the crate answered `2026-08-08T15` where the server it
//! is replicating answers `2026-08-08THH24`.
//!
//! Fixing the second needs the quoted literal form, which is how PostgreSQL
//! spells the ISO separator and which the validator refused outright, so the
//! lowering became a left-to-right scan rather than a blind substitution.
//!
//! Every expectation was read off PostgreSQL 17 and executed on SQLite before
//! the fix, and the pairs that must agree are asserted as one value here: the
//! translated call is run and compared to the literal PostgreSQL answered.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);";
const INSTANT: &str = "2026-08-08 15:04:05";

fn translate(pg: &str) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(&format!("{TABLE}\n{pg}"))
        .map_err(|e| e.to_string())?
        .translate_to_sql(&Pg2SqliteOptions::default())
        .map_err(|e| e.to_string())
}

fn translate_err(pg: &str) -> String {
    translate(pg).expect_err("translation should be refused")
}

/// Translates `to_char(ts, template)`, runs the emitted call over one fixed
/// instant, and returns what SQLite answered.
fn formatted(template: &str) -> String {
    let statements =
        translate(&format!("SELECT to_char(ts, '{template}') FROM t;")).expect("translate");
    let probe = statements.last().expect("a statement").clone();

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements[..statements.len() - 1] {
        connection.execute_batch(&format!("{statement};")).expect("emitted setup");
    }
    connection.execute("INSERT INTO t (id, ts) VALUES (1, ?1)", [INSTANT]).expect("row");
    connection
        .query_row(&probe, [], |row| row.get::<_, Option<String>>(0))
        .unwrap_or_else(|error| panic!("emitted probe failed: {probe}: {error}"))
        .unwrap_or_else(|| panic!("emitted probe answered NULL: {probe}"))
}

// --- F33: the two-digit year -----------------------------------------------

/// SQLite has no `%y`, so this used to emit a call answering NULL where
/// PostgreSQL answers `26`.
#[test]
fn the_two_digit_year_is_refused() {
    let error = translate_err("SELECT to_char(ts, 'YY') FROM t;");
    assert!(error.contains("YY"), "{error}");
    assert!(error.contains("YYYY"), "the message points at the four-digit year: {error}");
}

#[test]
fn the_two_digit_year_is_refused_inside_a_longer_template() {
    let error = translate_err("SELECT to_char(ts, 'YY-MM-DD') FROM t;");
    assert!(error.contains("YY"), "{error}");
}

/// The scan must not read the first two characters of `YYYY` as `YY`.
#[test]
fn the_four_digit_year_still_translates() {
    assert_eq!(formatted("YYYY"), "2026");
}

// --- F34: the ISO separator ------------------------------------------------

/// PostgreSQL reads `TH` as an ordinal suffix, answering `2026-08-08THH24`,
/// which `strftime` cannot express at all.
#[test]
fn a_bare_t_before_an_hour_is_refused() {
    let error = translate_err("SELECT to_char(ts, 'YYYY-MM-DDTHH24') FROM t;");
    assert!(error.contains('T'), "{error}");
    assert!(error.contains("\"T\""), "the message names the quoted spelling: {error}");
}

/// PostgreSQL reads `TM` as the translation-mode prefix, answering
/// `2026-08-086`.
#[test]
fn a_bare_t_before_a_minute_is_refused() {
    let error = translate_err("SELECT to_char(ts, 'YYYY-MM-DDTMI') FROM t;");
    assert!(error.contains("\"T\""), "{error}");
}

/// PostgreSQL answers `2026T` here, so nothing diverges and it keeps working.
#[test]
fn a_trailing_t_is_still_a_literal() {
    assert_eq!(formatted("YYYYT"), "2026T");
}

/// PostgreSQL answers `T05` here.
#[test]
fn a_t_before_a_second_is_still_a_literal() {
    assert_eq!(formatted("TSS"), "T05");
}

#[test]
fn the_quoted_separator_translates() {
    assert_eq!(formatted(r#"YYYY-MM-DD"T"HH24:MI:SS"#), "2026-08-08T15:04:05");
}

/// A code inside a quoted run is text, not a code. PostgreSQL answers `MM 08`.
#[test]
fn a_quoted_run_is_not_scanned_for_codes() {
    assert_eq!(formatted(r#""MM" MM"#), "MM 08");
}

/// SQLite reads `%` as introducing a specifier and answers NULL for one it does
/// not know, so a quoted percent has to be doubled. PostgreSQL answers
/// `100% 2026`.
#[test]
fn a_quoted_percent_survives_into_the_format() {
    assert_eq!(formatted(r#""100%" YYYY"#), "100% 2026");
}

#[test]
fn an_unterminated_quote_is_refused() {
    let error = translate_err(r#"SELECT to_char(ts, 'YYYY"T') FROM t;"#);
    assert!(error.contains("quote"), "{error}");
}

#[test]
fn a_backslash_inside_a_quoted_run_is_refused() {
    let error = translate_err(r#"SELECT to_char(ts, '"a\"b" YYYY') FROM t;"#);
    assert!(error.contains('\\'), "{error}");
}

// --- the round trip the quoted form closes ---------------------------------

/// F20's reverse direction emits the quoted separator. With the forward
/// direction accepting it, the crate can read its own reverse output.
#[test]
fn the_quoted_separator_round_trips() {
    let schema = Pg2Sqlite::default().sql(TABLE).expect("parse").build_schema().expect("schema");
    let sqlite = translate(r#"SELECT to_char(ts, 'YYYY-MM-DD"T"HH24') FROM t;"#)
        .expect("translate")
        .pop()
        .expect("a statement");
    assert!(sqlite.contains("strftime('%Y-%m-%dT%H', ts)"), "{sqlite}");

    let back = Pg2Sqlite::default()
        .reverse_sql(&sqlite, &schema, &Pg2SqliteOptions::default())
        .expect("reverse")
        .iter()
        .map(ToString::to_string)
        .collect::<String>();
    assert!(back.contains(r#"to_char(ts, 'YYYY-MM-DD"T"HH24')"#), "{back}");

    let again = translate(&format!("{back};")).expect("the reverse output translates again");
    assert!(
        again.last().expect("a statement").contains("strftime('%Y-%m-%dT%H', ts)"),
        "{again:?}"
    );
}
