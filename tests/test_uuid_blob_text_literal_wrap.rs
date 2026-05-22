//! Red tests: text UUID literals (and the PG `'...'::uuid` cast) at
//! INSERT/UPDATE positions targeting a UUID column must be wrapped
//! with a binary-conversion call when
//! `with_uuid_representation(Blob)` is in effect.
//!
//! The translator already emits `BLOB STRICT` for the UUID column,
//! so STRICT rejects raw TEXT at apply time. This is the direct
//! analogue of the pgvector text-literal wrap implemented in
//! `src/impls/translator_impls/{vector.rs,insert.rs}` and
//! `src/impls/shared_helpers.rs::translate_update`; a UUID-shaped
//! equivalent is missing.
//!
//! These tests are intentionally RED. They define the behaviour the
//! fix has to produce: INSERT and UPDATE of a textual UUID literal,
//! and the `::uuid` cast, must apply against the translated schema
//! without registering any additional UDF. The conversion strategy
//! (parse-time emission of `x'...'`, an unhex-based expression, a
//! pseudo UDF, ...) is intentionally not prescribed.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, TranslationOptions, UuidRepresentation};
use rusqlite::Connection;

const UUID_HEX_STR: &str = "550e8400-e29b-41d4-a716-446655440000";
const UUID_BYTES: [u8; 16] = [
    0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44, 0x00, 0x00,
];

fn blob_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob)
}

fn translate(schema: &str, opts: &Pg2SqliteOptions) -> String {
    let stmts =
        Pg2Sqlite::default().sql(schema).expect("parse").translate(opts).expect("translate");
    stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n")
}

#[test]
fn insert_uuid_text_literal_wraps_for_blob_representation() {
    let schema = format!(
        "CREATE TABLE u (id UUID PRIMARY KEY, name TEXT NOT NULL);\n\
         INSERT INTO u (id, name) VALUES ('{UUID_HEX_STR}', 'alice');"
    );
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&translate(&schema, &blob_opts())).expect(
        "INSERT of a UUID text literal must apply against the translated BLOB STRICT table",
    );
    let row: Vec<u8> =
        conn.query_row("SELECT id FROM u WHERE name = 'alice'", [], |r| r.get(0)).unwrap();
    assert_eq!(row, UUID_BYTES, "stored UUID must be the 16-byte binary form");
}

#[test]
fn insert_uuid_pg_cast_wraps_for_blob_representation() {
    let schema = format!(
        "CREATE TABLE u (id UUID PRIMARY KEY, name TEXT NOT NULL);\n\
         INSERT INTO u (id, name) VALUES ('{UUID_HEX_STR}'::uuid, 'alice');"
    );
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&translate(&schema, &blob_opts())).expect(
        "INSERT of a `'...'::uuid` cast must translate into a binary-conversion call and apply",
    );
    let row: Vec<u8> =
        conn.query_row("SELECT id FROM u WHERE name = 'alice'", [], |r| r.get(0)).unwrap();
    assert_eq!(row, UUID_BYTES, "stored UUID must be the 16-byte binary form");
}

#[test]
fn update_uuid_text_literal_wraps_for_blob_representation() {
    let schema = "CREATE TABLE u (id UUID PRIMARY KEY, name TEXT NOT NULL);".to_string();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&translate(&schema, &blob_opts())).expect("apply schema");
    // Seed via a parameter so the test does not depend on the INSERT
    // wrap (covered by the INSERT-side tests above).
    conn.execute("INSERT INTO u (id, name) VALUES (?1, 'alice')", [&UUID_BYTES[..]])
        .expect("seed via parameter");

    // PG-style UPDATE that targets the UUID column with a text literal.
    let pg_update = format!("UPDATE u SET id = '{UUID_HEX_STR}' WHERE name = 'alice';");
    let combined = format!("{schema}\n{pg_update}");
    let combined_stmts = Pg2Sqlite::default()
        .sql(&combined)
        .expect("parse")
        .translate(&blob_opts())
        .expect("translate");
    let schema_stmts = Pg2Sqlite::default()
        .sql(&schema)
        .expect("parse")
        .translate(&blob_opts())
        .expect("translate");
    let update_sql = combined_stmts
        .into_iter()
        .skip(schema_stmts.len())
        .map(|s| format!("{s};"))
        .collect::<Vec<_>>()
        .join("\n");
    conn.execute_batch(&update_sql).expect(
        "UPDATE setting a UUID column to a text literal must wrap with a binary-conversion call \
         and apply against the BLOB STRICT column",
    );

    let row: Vec<u8> =
        conn.query_row("SELECT id FROM u WHERE name = 'alice'", [], |r| r.get(0)).unwrap();
    assert_eq!(row, UUID_BYTES, "UPDATEd UUID must still be the 16-byte binary form");
}

#[test]
fn check_constraint_rejects_short_blob_under_blob_representation() {
    // Parameterised inserts that bind a non-16-byte BLOB bypass the
    // translate-time text-literal wrap. The column-level CHECK that
    // pg2sqlite attaches under `UuidRepresentation::Blob` must catch
    // them at apply time so a malformed UUID never lands in the table.
    let schema = "CREATE TABLE u (id UUID PRIMARY KEY, name TEXT NOT NULL);";
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&translate(schema, &blob_opts())).expect("apply schema");
    let short: [u8; 4] = [0, 1, 2, 3];
    let err = conn
        .execute("INSERT INTO u (id, name) VALUES (?1, 'alice')", [&short[..]])
        .expect_err("4-byte blob must trip the length(id) = 16 CHECK constraint");
    assert!(
        err.to_string().to_lowercase().contains("check"),
        "expected CHECK constraint failure, got: {err}"
    );
}

#[test]
fn uuid_text_to_blob_function_name_override_is_honoured() {
    // With a custom UDF name configured, the translator must emit a
    // call to that function instead of the default
    // unhex(replace(literal, '-', '')) shape.
    let schema = format!(
        "CREATE TABLE u (id UUID PRIMARY KEY, name TEXT NOT NULL);\n\
         INSERT INTO u (id, name) VALUES ('{UUID_HEX_STR}', 'alice');"
    );
    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_text_to_blob_function_name("uuid_text_to_blob");
    let translated = translate(&schema, &opts);
    assert!(
        translated.contains("uuid_text_to_blob('"),
        "expected custom UDF call in output, got:\n{translated}"
    );
    assert!(
        !translated.contains("unhex(replace("),
        "default unhex+replace must not appear when a UDF is configured, got:\n{translated}"
    );
}

#[test]
fn uuid_cast_in_select_lowers_to_conversion_call() {
    // `'...'::uuid` outside an INSERT (e.g. in a SELECT) must lower to
    // the same conversion expression as the INSERT/UPDATE wrap, never
    // to the invalid `'...'::BLOB` shape.
    let opts = blob_opts();
    let translated = Pg2Sqlite::default()
        .sql(&format!("SELECT '{UUID_HEX_STR}'::uuid AS id;"))
        .expect("parse")
        .translate(&opts)
        .expect("translate");
    let stmt = translated[0].to_string();
    assert!(
        !stmt.contains("::BLOB") && !stmt.contains("::Blob") && !stmt.contains("::uuid"),
        "translated SELECT must not contain a PG-style `::` cast, got: {stmt}"
    );
    assert!(
        stmt.contains("unhex") || stmt.contains("uuid_text_to_blob"),
        "translated SELECT must invoke the text-to-blob conversion, got: {stmt}"
    );
}

#[test]
fn text_representation_must_not_wrap() {
    // Sanity: when the UUID representation is Text, no wrap should
    // happen. Stored value is a 36-char string, not a BLOB.
    let schema = format!(
        "CREATE TABLE u (id UUID PRIMARY KEY, name TEXT NOT NULL);\n\
         INSERT INTO u (id, name) VALUES ('{UUID_HEX_STR}', 'alice');"
    );
    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Text);
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&translate(&schema, &opts)).expect("apply schema with Text rep");
    let row: String =
        conn.query_row("SELECT id FROM u WHERE name = 'alice'", [], |r| r.get(0)).unwrap();
    assert_eq!(row, UUID_HEX_STR, "Text-represented UUID must be stored as the 36-char string");
}
