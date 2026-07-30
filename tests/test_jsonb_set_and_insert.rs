//! `jsonb_set` and `jsonb_insert` need their arguments translated, not just
//! their names.
//!
//! Two defects came from the name-only rename, and the second is worse than the
//! one the review found. PostgreSQL takes a `text[]` path where SQLite takes a
//! JSONPath string, so `json_set(payload, '{a}', '2')` fails with `bad JSON
//! path: '{a}'`. And PostgreSQL's value argument is `jsonb`, so `'2'` is the
//! NUMBER 2, while SQLite's `json_set` treats a text argument as a STRING:
//! measured, `json_set('{"a":1}', '$.a', '2')` yields `{"a":"2"}` where
//! PostgreSQL yields `{"a": 2}`. The value has to be wrapped in `json(...)`.
//!
//! The mapping of the fourth argument was measured against both databases:
//!
//! | PostgreSQL | SQLite |
//! | --- | --- |
//! | `jsonb_set(t, p, v)` | `json_set` |
//! | `jsonb_set(t, p, v, false)` | `json_replace`, which leaves a missing path alone exactly as PostgreSQL does |
//! | `jsonb_insert(t, p, v)` | `json_insert` |
//! | `jsonb_insert(t, p, v, true)` | no equivalent, since SQLite cannot insert after an array element |

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

fn translate(pg: &str) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    Pg2Sqlite::default().sql(pg)?.translate_to_sql(&Pg2SqliteOptions::default())
}

/// Runs every emitted statement but the last, then returns the last one's first
/// column. Executing the translator's own output is what makes these proofs.
fn run_translated(pg: &str) -> String {
    let mut statements = translate(pg).expect("translate");
    let probe = statements.pop().expect("at least one statement");

    let conn = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        conn.execute_batch(&format!("{statement};"))
            .unwrap_or_else(|e| panic!("emitted setup failed: {e}\n{statement}"));
    }
    conn.query_row(&probe, [], |row| row.get::<_, String>(0))
        .unwrap_or_else(|e| panic!("emitted probe failed: {e}\n{probe}"))
}

fn translate_err(pg: &str) -> String {
    translate(pg).expect_err("expected a translation error").to_string()
}

/// The path becomes JSONPath and the document is updated.
#[test]
fn jsonb_set_updates_the_document() {
    assert_eq!(run_translated("SELECT jsonb_set('{\"a\":1}', '{a}', '2');"), r#"{"a":2}"#);
}

/// The value is JSON, not text. Without the `json(...)` wrapper this returns
/// `{"a":"2"}`, which is a wrong result rather than a failure.
#[test]
fn jsonb_set_keeps_the_value_typed_as_json() {
    let updated = run_translated("SELECT jsonb_set('{\"a\":1}', '{a}', '2');");
    assert!(!updated.contains(r#""2""#), "the value must stay a number, got {updated}");
}

/// A nested path joins with dots.
#[test]
fn a_nested_path_is_translated() {
    assert_eq!(
        run_translated("SELECT jsonb_set('{\"a\":{\"b\":1}}', '{a,b}', '2');"),
        r#"{"a":{"b":2}}"#
    );
}

/// `create_if_missing` defaults to true, so a missing key is added.
#[test]
fn jsonb_set_creates_a_missing_key_by_default() {
    assert_eq!(run_translated("SELECT jsonb_set('{\"a\":1}', '{b}', '2');"), r#"{"a":1,"b":2}"#);
}

/// `create_if_missing = false` is `json_replace`, which leaves a missing path
/// untouched, matching PostgreSQL rather than erroring.
#[test]
fn create_if_missing_false_becomes_json_replace() {
    assert_eq!(run_translated("SELECT jsonb_set('{\"a\":1}', '{b}', '2', false);"), r#"{"a":1}"#);
    assert_eq!(run_translated("SELECT jsonb_set('{\"a\":1}', '{a}', '2', true);"), r#"{"a":2}"#);
}

/// `jsonb_insert` adds a key that is absent.
#[test]
fn jsonb_insert_adds_a_missing_key() {
    assert_eq!(run_translated("SELECT jsonb_insert('{\"a\":1}', '{b}', '2');"), r#"{"a":1,"b":2}"#);
}

/// `insert_after` has no SQLite counterpart, so it is refused rather than
/// ignored.
#[test]
fn insert_after_is_rejected() {
    let error = translate_err("SELECT jsonb_insert('{\"a\":[1,2]}', '{a,0}', '9', true);");
    assert!(!error.is_empty(), "expected a rejection");
}

/// A numeric path element is ambiguous: PostgreSQL reads it as an array index
/// or an object key depending on the document at run time, and JSONPath has to
/// choose one at translation time. Both readings were measured against
/// PostgreSQL 16, `'{arr,0}'` indexing an array and `'{0}'` naming the key
/// `"0"`, so it is refused rather than guessed.
#[test]
fn a_numeric_path_element_is_rejected() {
    let error = translate_err("SELECT jsonb_set('{\"arr\":[1,2]}', '{arr,0}', '9');");
    assert!(
        error.contains("arr,0") || error.to_lowercase().contains("index"),
        "expected the error to explain the ambiguity, got {error}"
    );
}

/// A path that is not a literal cannot be converted at translation time.
#[test]
fn a_non_literal_path_is_rejected() {
    let error =
        translate_err("CREATE TABLE t (p TEXT[], d TEXT); SELECT jsonb_set(d, p, '2') FROM t;");
    assert!(!error.is_empty(), "expected a rejection");
}

/// The `ARRAY['a','b']` spelling of the path is the same thing and must work
/// too.
#[test]
fn an_array_literal_path_is_translated() {
    assert_eq!(
        run_translated("SELECT jsonb_set('{\"a\":{\"b\":1}}', ARRAY['a','b'], '2');"),
        r#"{"a":{"b":2}}"#
    );
}
