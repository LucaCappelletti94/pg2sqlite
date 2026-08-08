//! `array_to_string` over an array with nothing to join returns the empty
//! string, and over a NULL array returns NULL.
//!
//! The lowering is `group_concat` over `json_each`, and `group_concat` of zero
//! rows is NULL, so an empty array came back NULL where PostgreSQL answers the
//! empty string. An array holding only NULLs has the same shape, since both
//! engines skip a NULL element.
//!
//! Wrapping the aggregate in `coalesce(..., '')` alone is not the fix: a NULL
//! array also yields zero rows, so it would answer the empty string where
//! PostgreSQL answers NULL, trading one wrong answer for another. The array
//! itself is what tells the two apart.
//!
//! Every expected value below was read off PostgreSQL 17.

use pg2sqlite::{
    prelude::{ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};
use rusqlite::{Connection, types::FromSql};

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, tags TEXT[]);";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json)
}

fn translate(pg: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(&format!("{TABLE}\n{pg}"))
        .expect("parse")
        .translate_to_sql(&options())
        .expect("translate")
}

/// Applies everything but the last statement, then reads the last one's first
/// column.
fn evaluate<T: FromSql>(pg: &str) -> T {
    let mut statements = translate(pg);
    let probe = statements.pop().expect("a probe statement");
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }
    connection
        .query_row(&probe, [], |row| row.get::<_, T>(0))
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"))
}

/// PostgreSQL answers the empty string, of length zero. This used to be NULL.
#[test]
fn an_empty_array_joins_to_the_empty_string() {
    assert_eq!(
        evaluate::<Option<String>>("SELECT array_to_string(ARRAY[]::text[], ',');"),
        Some(String::new())
    );
}

/// Both engines skip a NULL element, so an array of nothing but NULLs has
/// nothing to join and PostgreSQL answers the empty string.
#[test]
fn an_array_of_only_nulls_joins_to_the_empty_string() {
    assert_eq!(
        evaluate::<Option<String>>("SELECT array_to_string(ARRAY[NULL, NULL]::text[], ',');"),
        Some(String::new())
    );
}

/// PostgreSQL answers NULL, which is what the plain `coalesce` fix would have
/// broken.
#[test]
fn a_null_array_stays_null() {
    assert_eq!(evaluate::<Option<String>>("SELECT array_to_string(NULL::text[], ',');"), None);
}

#[test]
fn a_populated_array_is_unchanged() {
    assert_eq!(
        evaluate::<Option<String>>("SELECT array_to_string(ARRAY['a', 'b'], ',');"),
        Some("a,b".to_string())
    );
}

/// PostgreSQL skips the NULL rather than joining an empty slot, so the answer
/// carries one separator, not two.
#[test]
fn a_null_element_is_skipped_rather_than_joined() {
    assert_eq!(
        evaluate::<Option<String>>("SELECT array_to_string(ARRAY['a', NULL, 'b'], ',');"),
        Some("a,b".to_string())
    );
}

/// The same five answers when the array comes from a column rather than a
/// literal, which is the shape a real schema uses and the one where the NULL
/// case is reachable.
#[test]
fn a_column_holding_each_case_answers_as_postgresql_does() {
    let statements = translate(
        "INSERT INTO t (id, tags) VALUES
             (1, ARRAY[]::text[]), (2, ARRAY['a', 'b']), (3, NULL), (4, ARRAY[NULL, NULL]::text[]);
         SELECT array_to_string(tags, ',') FROM t ORDER BY id;",
    );
    let (probe, setup) = statements.split_last().expect("a probe statement");
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in setup {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }
    let mut prepared = connection.prepare(probe).expect("prepare the probe");
    let answers: Vec<Option<String>> = prepared
        .query_map([], |row| row.get::<_, Option<String>>(0))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("read every row");
    assert_eq!(
        answers,
        vec![Some(String::new()), Some("a,b".to_string()), None, Some(String::new())]
    );
}
