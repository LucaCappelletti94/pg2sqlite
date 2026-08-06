//! `json_agg` over a JSON-typed column.
//!
//! Measured on PostgreSQL 16 over `(1, '{"a":1}', 'hello', 5)` and
//! `(2, '[2,3]', NULL, NULL)`: `json_agg(payload)` on a `jsonb` column is
//! `[{"a": 1}, [2, 3]]`, `json_agg(note)` on `text` is `["hello", null]`,
//! `json_agg(n)` on `int` is `[5, null]`, and any of them over zero rows is
//! NULL.
//!
//! Measured on SQLite 3.51.1: `json_group_array(payload)` over the same rows is
//! `["{\"a\":1}","[2,3]"]`, the double encoding this covers, while
//! `json_group_array(json(payload))` is `[{"a":1},[2,3]]`.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::Pg2SqliteOptions;
use run_translated_helper::run_translated_with;

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, payload JSONB, note TEXT, n INT);
     INSERT INTO t VALUES (1, '{\"a\":1}', 'hello', 5), (2, '[2,3]', NULL, NULL);";

fn probe(expression: &str) -> Vec<Option<String>> {
    run_translated_with(
        &format!("{TABLE} SELECT {expression} FROM t;"),
        &Pg2SqliteOptions::default(),
    )
}

#[test]
fn aggregating_a_json_column_nests_the_documents() {
    assert_eq!(probe("json_agg(payload)"), vec![Some(r#"[{"a":1},[2,3]]"#.to_string())]);
}

#[test]
fn jsonb_agg_behaves_the_same() {
    assert_eq!(probe("jsonb_agg(payload)"), vec![Some(r#"[{"a":1},[2,3]]"#.to_string())]);
}

/// A text column must keep being quoted, since its contents are a string and
/// not a document. `json()` over `hello` is `malformed JSON`, so a rewrite that
/// applies it to every argument fails loudly here rather than silently.
#[test]
fn aggregating_a_text_column_still_quotes() {
    assert_eq!(probe("json_agg(note)"), vec![Some(r#"["hello",null]"#.to_string())]);
}

#[test]
fn aggregating_a_numeric_column_is_unchanged() {
    assert_eq!(probe("json_agg(n)"), vec![Some("[5,null]".to_string())]);
}

/// PostgreSQL answers NULL for an aggregate over no rows, SQLite's
/// `json_group_array` answers `[]`. An aggregate over one or more rows always
/// has an element, so an empty array can only mean zero rows.
#[test]
fn aggregating_no_rows_is_null() {
    let rows = run_translated_with(
        &format!("{TABLE} SELECT json_agg(payload) FROM t WHERE id < 0;"),
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![None]);
}

/// A qualified reference resolves to the same column.
#[test]
fn a_qualified_json_column_nests_too() {
    assert_eq!(probe("json_agg(t.payload)"), vec![Some(r#"[{"a":1},[2,3]]"#.to_string())]);
}

/// The document predicate is shared with `to_json` (R89), so an array column
/// under the JSON representation nests as an array of arrays rather than an
/// array of strings.
#[test]
fn aggregating_an_array_column_nests_the_arrays() {
    use pg2sqlite::prelude::{ArrayRepresentation, TranslationOptions};

    let rows = run_translated_with(
        "CREATE TABLE arr (id INT PRIMARY KEY, tags INT[]);
         INSERT INTO arr (id, tags) VALUES (1, ARRAY[1, 2]);
         SELECT json_agg(tags) FROM arr;",
        &Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json),
    );
    assert_eq!(rows, vec![Some("[[1,2]]".to_string())]);
}
