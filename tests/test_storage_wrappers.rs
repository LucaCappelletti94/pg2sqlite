//! Storage wrappers on both sides of the translation, where the value's
//! PostgreSQL type is not the type it is stored as.
//!
//! Going out, a `bytea` hex literal was cast rather than decoded, so the
//! replica held the six characters of `\x00ff` instead of two bytes. Coming
//! back, `unhex`, `hex`, `json_array` and `json_array_length` were mapped by
//! name, which named the storage type: the server answered `operator does not
//! exist: uuid = bytea`, `cannot cast type uuid to bytea`, `column "xs" is of
//! type integer[] but expression is of type json` and `function
//! json_array_length(integer[]) does not exist`.
//!
//! `tests/gauntlet/reverse.rs` runs the reversed statements against a real
//! server and compares its answers with the replica's. This file pins the
//! forms without needing one.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};
use run_translated_helper::run_translated_with;

const DDL: &str =
    "CREATE TABLE stored (id int PRIMARY KEY, u uuid, raw bytea, xs int[], doc jsonb);";

/// Options naming both storage representations, which a uuid or array column
/// needs in either direction.
fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_array_representation(ArrayRepresentation::Json)
}

/// The PostgreSQL `sqlite` reverses to.
fn reversed(sqlite: &str) -> String {
    let schema =
        Pg2Sqlite::default().sql(DDL).expect("the schema parses").build_schema().expect("builds");
    Pg2Sqlite::default()
        .reverse_sql(sqlite, &schema, &options())
        .expect("the statement reverses")
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// The refusal `sqlite` earns coming back.
fn reverse_refusal(sqlite: &str, reverse_options: &Pg2SqliteOptions) -> String {
    let schema =
        Pg2Sqlite::default().sql(DDL).expect("the schema parses").build_schema().expect("builds");
    Pg2Sqlite::default()
        .reverse_sql(sqlite, &schema, reverse_options)
        .expect_err("expected a refusal")
        .to_string()
}

#[test]
fn a_bytea_hex_literal_is_decoded_rather_than_cast() {
    // Measured: `CAST('\x00ff' AS BLOB)` stored the six characters, so
    // `hex(raw)` answered 5C7830306666 where PostgreSQL answers 00FF.
    assert_eq!(
        run_translated_with(
            &format!(
                "{DDL} INSERT INTO stored (id, raw) VALUES (1, '\\x00ff'::bytea); \
                 SELECT hex(raw) FROM stored;"
            ),
            &options()
        ),
        vec![Some("00FF".to_string())]
    );
}

#[test]
fn a_bytea_hex_literal_beside_the_column_is_decoded_too() {
    // The bare literal is how PostgreSQL is usually written, and it never
    // matched: a text value compared with a blob is never equal in SQLite.
    assert_eq!(
        run_translated_with(
            &format!(
                "{DDL} INSERT INTO stored (id, raw) VALUES (1, '\\x00ff'); \
                 SELECT count(*) FROM stored WHERE raw = '\\x00ff';"
            ),
            &options()
        ),
        vec![Some("1".to_string())]
    );
}

#[test]
fn hex_over_a_uuid_column_reverses_to_the_undashed_digits() {
    // SQLite hexes the 16 raw bytes, which is the uuid's own digits without
    // the dashes: both engines answer 550E8400E29B41D4A716446655440000. The
    // bytea cast the old form used earns `cannot cast type uuid to bytea`.
    let postgres = reversed("SELECT hex(u) FROM stored");
    assert!(postgres.contains("replace"), "{postgres}");
    assert!(!postgres.contains("BYTEA"), "{postgres}");
}

#[test]
fn hex_over_a_bytea_column_still_encodes_the_bytes() {
    let postgres = reversed("SELECT hex(raw) FROM stored");
    assert!(postgres.contains("encode(raw::BYTEA, 'hex')"), "{postgres}");
}

#[test]
fn a_hex_blob_beside_a_uuid_column_reverses_to_a_uuid_cast() {
    // PostgreSQL takes the 32 hex digits as a uuid, measured, where the
    // decode form earns `operator does not exist: uuid = bytea`.
    let postgres =
        reversed("SELECT id FROM stored WHERE u = unhex('550e8400e29b41d4a716446655440000')");
    assert!(postgres.contains("'550e8400e29b41d4a716446655440000'::UUID"), "{postgres}");
    assert!(!postgres.contains("decode"), "{postgres}");
}

#[test]
fn a_hex_blob_beside_a_bytea_column_still_decodes() {
    let postgres = reversed("SELECT id FROM stored WHERE raw = unhex('00ff')");
    assert!(postgres.contains("decode('00ff', 'hex')"), "{postgres}");
}

#[test]
fn a_hex_blob_beside_a_uuid_column_held_as_text_is_refused() {
    // SQLite compares a blob with text and matches nothing, and no PostgreSQL
    // expression answers that.
    let text_options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Text)
        .with_array_representation(ArrayRepresentation::Json);
    let message = reverse_refusal(
        "SELECT id FROM stored WHERE u = unhex('550e8400e29b41d4a716446655440000')",
        &text_options,
    );
    assert!(message.contains("uuid"), "{message}");
    assert!(message.contains("text"), "{message}");
}

#[test]
fn a_hex_blob_beside_a_uuid_column_with_no_representation_is_refused() {
    // The reversal depends on the representation, and nothing names one here,
    // which is also why the forward direction refuses a uuid column outright.
    let message = reverse_refusal(
        "SELECT id FROM stored WHERE u = unhex('550e8400e29b41d4a716446655440000')",
        &Pg2SqliteOptions::default(),
    );
    assert!(message.contains("representation"), "{message}");
}

#[test]
fn a_json_array_beside_an_array_column_reverses_to_an_array_constructor() {
    // `xs = json_build_array(1, 2)` earns `operator does not exist:
    // integer[] = json` on the server.
    let postgres = reversed("SELECT id FROM stored WHERE xs = json_array(1, 2)");
    assert!(postgres.contains("ARRAY[1, 2]"), "{postgres}");
    assert!(!postgres.contains("json_build_array"), "{postgres}");
}

#[test]
fn a_json_array_beside_a_json_column_still_builds_json() {
    let postgres = reversed("SELECT id FROM stored WHERE doc = json_array(1, 2)");
    assert!(postgres.contains("json_build_array(1, 2)"), "{postgres}");
}

#[test]
fn an_empty_json_array_beside_an_array_column_carries_the_column_type() {
    // PostgreSQL takes no untyped empty array constructor: `ARRAY[]` alone is
    // `cannot determine type of empty array`.
    let postgres = reversed("SELECT id FROM stored WHERE xs = json_array()");
    assert!(postgres.contains("ARRAY[]::INT[]"), "{postgres}");
}

#[test]
fn the_length_of_an_array_column_reverses_to_array_length() {
    // `json_array_length(integer[])` does not exist on the server, and
    // `array_length` answers NULL for an empty array where SQLite answers 0,
    // so the zero is restored.
    let postgres = reversed("SELECT json_array_length(xs) FROM stored");
    assert!(postgres.contains("array_length(xs, 1)"), "{postgres}");
    assert!(postgres.contains("coalesce"), "{postgres}");
}

#[test]
fn the_length_of_a_json_column_still_reverses_to_the_json_overload() {
    let postgres = reversed("SELECT json_array_length(doc) FROM stored");
    assert!(postgres.contains("jsonb_array_length(doc)"), "{postgres}");
}

/// A reference the schema cannot resolve leaves the wrapper undecided, so the
/// statement is refused rather than reversed by the storage type.
///
/// `unhex` over a column held as a blob is a uuid, a bytea or neither, and the
/// three reverse differently. Reading it as bytea because the relation is
/// absent emitted `decode(..., 'hex')` against what the server may hold as a
/// uuid, where it answers `operator does not exist: uuid = bytea`.
#[test]
fn an_unresolvable_reference_refuses_rather_than_guessing_the_storage_type() {
    let message = reverse_refusal(
        "SELECT id FROM absent WHERE u = unhex('550e8400e29b41d4a716446655440000')",
        &options(),
    );
    assert!(message.contains('u'), "the refusal names the reference: {message}");
}

/// `hex` over a reference the schema cannot answer keeps the `bytea` cast the
/// arm documents as its fallback, since that cast is a no-op for a `bytea`
/// column and a view's reverse translation is not worth giving up over a type
/// nobody could read.
#[test]
fn hex_over_an_unresolvable_reference_keeps_its_documented_fallback() {
    let schema =
        Pg2Sqlite::default().sql(DDL).expect("the schema parses").build_schema().expect("builds");
    let reversed = Pg2Sqlite::default()
        .reverse_sql("SELECT hex(u) FROM absent", &schema, &options())
        .expect("an unresolvable reference still reverses")
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    assert!(reversed.contains("encode(u::BYTEA, 'hex')"), "{reversed}");
}
