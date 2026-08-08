//! F20: PostgreSQL has no `strftime`, so passing one through is not an option.
//!
//! Three tables decide what a `strftime` call becomes. Six truncating formats
//! become `date_trunc`, thirteen single specifiers become `EXTRACT`, and a
//! format built from the specifiers the `to_char` lowering emits comes home as
//! `to_char`. Anything else is refused, along with a format that is not a
//! literal and a call carrying the extra modifier arguments SQLite allows.
//!
//! The `to_char` half matters because the crate produces those formats itself:
//! `to_char(ts, 'YYYY-MM-DD')` emits `strftime('%Y-%m-%d', ts)`, which had no
//! way home.
//!
//! Every expectation was read off PostgreSQL 17 and SQLite before the fix, and
//! the assertions pin the emitted call text. Parsing settles nothing here: the
//! broken output parsed and simply named a function PostgreSQL does not have.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

const FIXTURE: &str = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP);";

fn schema() -> ParserDB {
    Pg2Sqlite::default().sql(FIXTURE).expect("parse").build_schema().expect("build")
}

fn reverse(sqlite: &str) -> String {
    let statements = Pg2Sqlite::default()
        .reverse_sql(sqlite, &schema(), &Pg2SqliteOptions::default())
        .expect("reverse translation");
    let sql = statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    Parser::parse_sql(&PostgreSqlDialect {}, &sql).expect("output parses as PostgreSQL");
    sql
}

fn reverse_err(sqlite: &str) -> String {
    Pg2Sqlite::default()
        .reverse_sql(sqlite, &schema(), &Pg2SqliteOptions::default())
        .expect_err("reverse translation should be refused")
        .to_string()
}

fn forward(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{FIXTURE}\n{pg}"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .pop()
        .expect("at least one statement")
}

// --- formats with no PostgreSQL form --------------------------------------

/// The Sunday based week. `EXTRACT(WEEK)` is the ISO one and would answer
/// differently, so there is nothing to translate it to.
#[test]
fn a_format_outside_every_table_is_refused() {
    let error = reverse_err("SELECT strftime('%W', ts) FROM t");
    assert!(error.contains("%W"), "{error}");
    assert!(error.contains("date_trunc"), "the message lists what does reverse: {error}");
    assert!(error.contains("to_char"), "the message lists what does reverse: {error}");
}

#[test]
fn a_format_that_is_not_a_literal_is_refused() {
    let error = reverse_err("SELECT strftime(s, ts) FROM t");
    assert!(error.contains("strftime"), "{error}");
}

/// SQLite takes the same modifiers `datetime` takes after the timestamp.
#[test]
fn a_call_carrying_modifiers_is_refused() {
    let error = reverse_err("SELECT strftime('%Y', ts, 'utc') FROM t");
    assert!(error.contains("strftime"), "{error}");
}

/// SQLite has no `%y`, so this answers NULL where `to_char(ts, 'YY')` answers
/// two digits. Claiming they are the same would be worse than refusing.
#[test]
fn the_two_digit_year_is_refused_rather_than_mapped_to_yy() {
    let error = reverse_err("SELECT strftime('%y', ts) FROM t");
    assert!(error.contains("%y"), "{error}");
}

// --- formats the to_char lowering produces --------------------------------

#[test]
fn a_date_format_comes_home_as_to_char() {
    let sql = reverse("SELECT strftime('%Y-%m-%d', ts) FROM t");
    assert!(sql.contains("to_char(ts, 'YYYY-MM-DD')"), "{sql}");
}

#[test]
fn a_time_format_comes_home_as_to_char() {
    let sql = reverse("SELECT strftime('%H:%M:%S', ts) FROM t");
    assert!(sql.contains("to_char(ts, 'HH24:MI:SS')"), "{sql}");
}

/// PostgreSQL's `HH` means `HH12`, so the way back picks the unambiguous one.
#[test]
fn a_twelve_hour_format_comes_home_as_hh12() {
    let sql = reverse("SELECT strftime('%I:%M', ts) FROM t");
    assert!(sql.contains("to_char(ts, 'HH12:MI')"), "{sql}");
}

/// PostgreSQL reads a bare `T` as the start of `TH` or `TM`, so it is quoted.
#[test]
fn an_iso_separator_comes_home_quoted() {
    let sql = reverse("SELECT strftime('%Y-%m-%dT%H:%M:%S', ts) FROM t");
    assert!(sql.contains(r#"to_char(ts, 'YYYY-MM-DD"T"HH24:MI:SS')"#), "{sql}");
}

/// The crate emits this format itself, so this is it reading its own output.
#[test]
fn the_crate_reads_back_its_own_to_char_output() {
    let sqlite = forward("SELECT to_char(ts, 'YYYY-MM-DD') FROM t;");
    assert!(sqlite.contains("strftime('%Y-%m-%d', ts)"), "{sqlite}");
    let restored = reverse(&sqlite);
    assert!(restored.contains("to_char(ts, 'YYYY-MM-DD')"), "{restored}");
}

// --- the two tables that already worked -----------------------------------

#[test]
fn a_truncating_format_still_becomes_date_trunc() {
    let sql = reverse("SELECT strftime('%Y-01-01 00:00:00', ts) FROM t");
    assert!(sql.contains("date_trunc('year', ts)"), "{sql}");
}

#[test]
fn a_single_specifier_still_becomes_extract() {
    let sql = reverse("SELECT strftime('%Y', ts) FROM t");
    assert!(sql.contains("EXTRACT(YEAR FROM ts)"), "{sql}");
}

/// A composite the `date_trunc` table claims stays with `date_trunc` rather
/// than moving to `to_char`, since nothing else changed about it.
#[test]
fn a_truncating_format_is_not_claimed_by_to_char() {
    let sql = reverse("SELECT strftime('%Y-%m-%d %H:%M:%S', ts) FROM t");
    assert!(sql.contains("date_trunc('second', ts)"), "{sql}");
}
