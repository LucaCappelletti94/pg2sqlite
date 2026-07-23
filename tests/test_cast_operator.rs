//! PostgreSQL `::` casts must translate to SQLite's `CAST(x AS type)` form.
//!
//! SQLite has no `::` operator (nor `TRY_CAST` / `SAFE_CAST`), so every cast
//! the translator emits has to use the `CAST(...)` spelling. See
//! docs/double_colon_cast_translation.md.

mod helpers;

use diesel::{
    QueryableByName, connection::SimpleConnection, prelude::*, sql_query, sql_types::Text,
};
use helpers::{establish_connection, translate_pg};
use pg2sqlite::prelude::{Pg2SqliteOptions, TranslationOptions, UuidRepresentation};

/// Text-bound scalar result for the apply test.
#[derive(QueryableByName)]
struct Row {
    /// Cast output, bound by the `r` alias.
    #[diesel(sql_type = Text)]
    r: String,
}

fn tr(pg: &str) -> String {
    translate_pg(pg, &Pg2SqliteOptions::default()).expect("translation failed").join("\n")
}

#[test]
fn double_colon_text_cast_uses_cast_syntax() {
    let out = tr("SELECT id::text FROM t");
    assert!(out.contains("CAST(id AS TEXT)"), "{out}");
    assert!(!out.contains("::"), "cast operator leaked into output: {out}");
}

#[test]
fn double_colon_int_literal_cast_uses_cast_syntax() {
    let out = tr("SELECT '1'::int");
    assert!(out.contains("CAST('1' AS INTEGER)"), "{out}");
    assert!(!out.contains("::"), "cast operator leaked into output: {out}");
}

#[test]
fn double_colon_numeric_cast_maps_to_real() {
    let out = tr("SELECT val::numeric(10, 2) FROM t");
    assert!(out.contains("CAST(val AS REAL)"), "{out}");
    assert!(!out.contains("::"), "cast operator leaked into output: {out}");
}

#[test]
fn nested_double_colon_casts_have_no_operator() {
    let out = tr("SELECT (a::int)::text FROM t");
    assert!(!out.contains("::"), "nested cast operator leaked: {out}");
    assert!(out.matches("CAST(").count() >= 2, "expected two CAST calls: {out}");
}

#[test]
fn cast_output_runs_in_sqlite() {
    let mut conn = establish_connection();
    conn.batch_execute(&tr("CREATE TABLE c (id INTEGER PRIMARY KEY, n INTEGER)"))
        .expect("apply schema");
    conn.batch_execute("INSERT INTO c (id, n) VALUES (1, 42)").expect("seed");

    let out = tr("SELECT n::text AS r FROM c");
    assert!(!out.contains("::"), "{out}");
    let row: Row = sql_query(out).get_result(&mut conn).expect("cast output must run in SQLite");
    assert_eq!(row.r, "42");
}

#[test]
fn vector_cast_still_lowers_to_vec_f32() {
    // Regression: the pgvector special-case returns before the generic cast
    // arm, so forcing CastKind::Cast must not disturb it.
    let out = tr("SELECT '[1,2,3]'::vector FROM t");
    assert!(out.contains("vec_f32"), "{out}");
    assert!(!out.contains("::"), "{out}");
}

#[test]
fn uuid_blob_cast_still_lowers_to_conversion() {
    // Regression: `::uuid` under the Blob representation is rewritten by its
    // own branch, not the generic CAST path.
    let opts = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string());
    let out = translate_pg("SELECT '11111111-1111-1111-1111-111111111111'::uuid FROM t", &opts)
        .expect("translation failed")
        .join("\n");
    assert!(!out.contains("::"), "{out}");
    assert!(
        !out.to_uppercase().contains("AS UUID"),
        "uuid cast fell through to generic path: {out}"
    );
}
