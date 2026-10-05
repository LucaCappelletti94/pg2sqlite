//! F26: the README's `Semantic differences` section, held to its own claims.
//!
//! Every bullet states what one engine answers or what the crate emits.
//! Prose cannot check itself, so each claim about SQLite is executed here and
//! each claim about the emitted SQL is translated here. What is left unchecked
//! is the PostgreSQL half, which was measured on PostgreSQL 17 when the
//! section was written and is quoted in the plan.
//!
//! A README claim that stops being true is worse than no claim, because a
//! reader has no way to tell.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

/// What SQLite answers for `expression`, rendered the way SQLite renders it,
/// with `None` for SQL NULL.
///
/// The cast matters: reading the value through a host language would render
/// the float itself, and the README quotes what SQLite prints.
fn sqlite_answers(expression: &str) -> Option<String> {
    Connection::open_in_memory()
        .expect("in-memory SQLite")
        .query_row(&format!("SELECT CAST({expression} AS TEXT)"), [], |row| {
            row.get::<_, Option<String>>(0)
        })
        .expect("expression")
}

fn translate(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .pop()
        .expect("a statement")
}

// --- PostgreSQL raises, SQLite answers -------------------------------------

#[test]
fn dividing_by_zero_answers_null() {
    assert_eq!(sqlite_answers("1 / 0"), None);
}

#[test]
fn a_cast_of_unparsable_text_keeps_the_prefix() {
    assert_eq!(sqlite_answers("CAST('12abc' AS INTEGER)"), Some("12".to_string()));
}

#[test]
fn integer_overflow_degrades_to_a_float() {
    assert_eq!(
        sqlite_answers("9223372036854775807 + 1"),
        Some("9.2233720368547758e+18".to_string())
    );
    assert_eq!(sqlite_answers("typeof(9223372036854775807 + 1)"), Some("real".to_string()));
}

/// None of the three is translated away, which is why the section calls them
/// differences rather than defects.
#[test]
fn none_of_the_three_is_rewritten() {
    assert!(translate("SELECT 1 / 0;").contains("1 / 0"));
    assert!(translate("SELECT CAST('12abc' AS INTEGER);").contains("CAST('12abc' AS INTEGER)"));
    assert!(translate("SELECT 9223372036854775807 + 1;").contains("9223372036854775807 + 1"));
}

/// The exception the section names: a NUMERIC column is bounded, a BIGINT
/// column beside it is not.
#[test]
fn a_numeric_column_carries_the_bound_the_readme_quotes() {
    let ddl = translate("CREATE TABLE m (id INT PRIMARY KEY, amount NUMERIC(10,2), n BIGINT);");
    assert!(ddl.contains("CHECK (amount BETWEEN -9999999999 AND 9999999999)"), "{ddl}");
    assert!(!ddl.contains("CHECK (n"), "a plain BIGINT carries no bound: {ddl}");
}

// --- text comparison follows the collation ---------------------------------

#[test]
fn upper_case_sorts_before_lower_case() {
    assert_eq!(sqlite_answers("'a' < 'B'"), Some("0".to_string()));
}

/// The section says this reaches ordering, not only explicit comparisons.
#[test]
fn the_same_ordering_reaches_order_by() {
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    connection
        .execute_batch("CREATE TABLE t (s TEXT); INSERT INTO t VALUES ('a'), ('B');")
        .expect("fixture");
    let first: String = connection
        .query_row("SELECT s FROM t ORDER BY s LIMIT 1", [], |row| row.get(0))
        .expect("ordering");
    assert_eq!(first, "B");
}

// --- now() ------------------------------------------------------------------

/// What SQLite answers for the translation of the PostgreSQL `expression`.
fn translated_answer(expression: &str) -> String {
    let translated = translate(&format!("SELECT {expression};"));
    let projection = translated.strip_prefix("SELECT ").expect("a projection");
    sqlite_answers(projection).expect("a value")
}

#[test]
fn now_and_current_timestamp_answer_utc_text_with_microseconds() {
    for expression in ["now()", "CURRENT_TIMESTAMP"] {
        let answer = translated_answer(expression);
        assert!(answer.ends_with("+00:00"), "{expression}: {answer}");
        assert_eq!(
            answer.len(),
            "2026-08-08 15:08:14.123000+00:00".len(),
            "{expression}: {answer}"
        );
    }
}
