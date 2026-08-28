//! `char_length` and `character_length`, which PostgreSQL defines only over
//! text.
//!
//! Measured on PostgreSQL 16: `char_length` over `text`, `varchar`, and `char`
//! counts characters, and over anything else it does not exist at all.
//! `char_length(u)` on a `uuid` column answers `function char_length(uuid) does
//! not exist`, and the same for `bytea` and `integer`.
//!
//! That matters because SQLite's `length` accepts everything and counts BYTES
//! for a BLOB, so the rename turned a query PostgreSQL rejects into one that
//! quietly answers 16 for a UUID stored as a blob.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};
use run_translated_helper::run_translated_with;

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT, v VARCHAR(20), u UUID, b BYTEA);
     INSERT INTO t VALUES (1, 'héllo', 'abc', '550e8400-e29b-41d4-a716-446655440000', NULL);";

fn blob_uuids() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}

fn evaluate(expression: &str) -> Option<String> {
    run_translated_with(&format!("{TABLE} SELECT {expression} FROM t;"), &blob_uuids())
        .into_iter()
        .next()
        .expect("one row")
}

fn refuse(expression: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{TABLE} SELECT {expression} FROM t;"))
        .expect("parse")
        .translate(&blob_uuids())
        .expect_err("PostgreSQL has no char_length for this type")
        .to_string()
}

/// Characters, not bytes, so the accented letter counts once.
#[test]
fn a_text_column_is_counted_in_characters() {
    assert_eq!(evaluate("char_length(s)"), Some("5".to_string()));
    assert_eq!(evaluate("character_length(s)"), Some("5".to_string()));
    assert_eq!(evaluate("char_length(v)"), Some("3".to_string()));
}

/// A literal has no declared type to consult and is text by construction.
#[test]
fn a_literal_still_translates() {
    assert_eq!(evaluate("char_length('abcd')"), Some("4".to_string()));
}

/// The item's case. A UUID column is a BLOB under this representation, and
/// SQLite would have counted its 16 bytes. PostgreSQL refuses the query, so
/// this refuses it too rather than inventing an answer PostgreSQL never gives.
#[test]
fn a_uuid_column_is_refused() {
    let error = refuse("char_length(u)");
    assert!(error.contains("char_length"), "the error must name the function, got: {error}");
}

#[test]
fn a_bytea_column_is_refused() {
    let error = refuse("character_length(b)");
    assert!(
        error.contains("character_length"),
        "the error must name the function as written, got: {error}"
    );
}

/// A non-textual column that is not binary either, so the refusal is about the
/// declared type rather than about blobs.
#[test]
fn an_integer_column_is_refused() {
    let error = refuse("char_length(id)");
    assert!(error.contains("char_length"), "got: {error}");
}
