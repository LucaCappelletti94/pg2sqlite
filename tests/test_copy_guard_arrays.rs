//! Refusal tests for array and ASCII lowerings that name an operand more than
//! once.
//!
//! A lowering that copies an operand into multiple output positions evaluates
//! it more times than PostgreSQL does. A volatile operand (one whose value may
//! differ between calls) would then answer from draws PostgreSQL never made.
//! Each test pair checks the refusal over `random()` and execution over a
//! stable operand.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

fn json_arrays() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json)
}

fn reject_json(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&json_arrays())
        .expect_err("translation should be rejected")
        .to_string()
}

fn run_json(pg: &str) -> Vec<Option<String>> {
    run_translated_with(pg, &json_arrays())
}

// ── array subscript ─────────────────────────────────────────────────────────

/// The non-literal index guard reads it in the bounds check and again in the
/// path. A volatile index is refused.
#[test]
fn subscript_volatile_index_is_refused() {
    let err = reject_json("SELECT (ARRAY['a','b','c'])[random()::int];");
    assert!(err.contains("array subscript"), "error should name the construct: {err}");
}

/// A column-reference index is stable and still translates correctly.
#[test]
fn subscript_stable_column_index_translates() {
    let rows = run_json(
        "CREATE TABLE t (idx INT); \
         INSERT INTO t VALUES (2); \
         SELECT (ARRAY['a','b','c'])[idx] FROM t;",
    );
    assert_eq!(rows, vec![Some("b".to_string())]);
}

// ── array_to_string ──────────────────────────────────────────────────────────

/// The array appears in the IS NOT NULL guard and inside the subquery body.
/// A volatile element inside the array is refused.
#[test]
fn array_to_string_volatile_array_is_refused() {
    let err = reject_json("SELECT array_to_string(ARRAY[random()::text], ',');");
    assert!(err.contains("array_to_string"), "error should name the construct: {err}");
}

/// A literal array still joins into a string.
#[test]
fn array_to_string_stable_array_translates() {
    let rows = run_json("SELECT array_to_string(ARRAY['hello', 'world'], ', ');");
    assert_eq!(rows, vec![Some("hello, world".to_string())]);
}

// ── array_cat ────────────────────────────────────────────────────────────────

/// The first array is checked for NULL and passed into the concat body.
/// A volatile element in the first array is refused.
#[test]
fn array_cat_volatile_first_array_is_refused() {
    let err = reject_json("SELECT array_cat(ARRAY[random()::text], ARRAY['x']);");
    assert!(err.contains("array_cat"), "error should name the construct: {err}");
}

/// The second array is also checked and passed into the body.
/// A volatile element in the second array is refused.
#[test]
fn array_cat_volatile_second_array_is_refused() {
    let err = reject_json("SELECT array_cat(ARRAY['x'], ARRAY[random()::text]);");
    assert!(err.contains("array_cat"), "error should name the construct: {err}");
}

/// Two literal arrays concatenate into a single JSON array.
#[test]
fn array_cat_stable_arrays_translate() {
    let rows = run_json("SELECT array_cat(ARRAY['a', 'b'], ARRAY['c', 'd']);");
    assert_eq!(rows, vec![Some("[\"a\",\"b\",\"c\",\"d\"]".to_string())]);
}

// ── array_append ─────────────────────────────────────────────────────────────

/// When the input array is NULL, the appended value appears in both branches of
/// the COALESCE. A volatile value is refused.
#[test]
fn array_append_volatile_value_is_refused() {
    let err = reject_json("SELECT array_append(ARRAY['x'], random()::text);");
    assert!(err.contains("array_append"), "error should name the construct: {err}");
}

/// Appending a literal to a literal array gives a longer array.
#[test]
fn array_append_stable_value_translates() {
    let rows = run_json("SELECT array_append(ARRAY['a', 'b'], 'c');");
    assert_eq!(rows, vec![Some("[\"a\",\"b\",\"c\"]".to_string())]);
}

// ── array_positions ──────────────────────────────────────────────────────────

/// The array is in the IS NOT NULL guard and passed to json_each in the body.
/// A volatile element inside the array is refused.
#[test]
fn array_positions_volatile_array_is_refused() {
    let err = reject_json("SELECT array_positions(ARRAY[random()::text], 'x');");
    assert!(err.contains("array_positions"), "error should name the construct: {err}");
}

/// Finding all positions of a literal value in a literal array returns a JSON
/// array of one-based indices.
#[test]
fn array_positions_stable_array_translates() {
    let rows = run_json("SELECT array_positions(ARRAY['a', 'b', 'a'], 'a');");
    assert_eq!(rows, vec![Some("[1,3]".to_string())]);
}

// ── array_replace ────────────────────────────────────────────────────────────

/// The array is in the IS NOT NULL guard and passed to json_each in the body.
/// A volatile element inside the array is refused.
#[test]
fn array_replace_volatile_array_is_refused() {
    let err = reject_json("SELECT array_replace(ARRAY[random()::text], 'x', 'y');");
    assert!(err.contains("array_replace"), "error should name the construct: {err}");
}

/// Replacing every occurrence of a value in a literal array gives the expected
/// JSON array.
#[test]
fn array_replace_stable_array_translates() {
    let rows = run_json("SELECT array_replace(ARRAY['a', 'b', 'a'], 'a', 'c');");
    assert_eq!(rows, vec![Some("[\"c\",\"b\",\"c\"]".to_string())]);
}

// ── ascii ────────────────────────────────────────────────────────────────────

/// The argument appears in the empty-string check and in the unicode() branch.
/// A volatile argument is refused.
#[test]
fn ascii_volatile_argument_is_refused() {
    let err = Pg2Sqlite::default()
        .sql("SELECT ascii(random()::text);")
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("translation should be rejected")
        .to_string();
    assert!(err.contains("ascii"), "error should name the construct: {err}");
}

/// A literal string argument still returns the correct code point.
#[test]
fn ascii_stable_argument_translates() {
    let rows = run_translated_with("SELECT ascii('A');", &Pg2SqliteOptions::default());
    assert_eq!(rows, vec![Some("65".to_string())]);
}
