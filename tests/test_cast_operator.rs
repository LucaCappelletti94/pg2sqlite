//! PostgreSQL `::` casts must translate to SQLite's `CAST(x AS type)` form.
//!
//! SQLite has no `::` operator (nor `TRY_CAST` / `SAFE_CAST`), so every cast
//! the translator emits has to use the `CAST(...)` spelling. See
//! docs/double_colon_cast_translation.md.

mod helpers;

use std::sync::Once;

use diesel::{
    QueryableByName, connection::SimpleConnection, prelude::*, sql_query, sql_types::Text,
};
use helpers::{establish_connection, translate_pg};
use pg2sqlite::prelude::{Pg2SqliteOptions, UuidRepresentation};
use sqlite_vec::sqlite3_vec_init;

/// Register sqlite-vec once per process so connections opened by any test in
/// this binary have the extension loaded.
///
/// SAFETY: `sqlite3_vec_init` is the sqlite-vec C entry point with signature
/// `(db, pzErrMsg, pApi) -> int`. The transmute restores that type.
/// rusqlite is used here because diesel does not expose
/// `sqlite3_auto_extension`.
fn register_sqlite_vec_once() {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(sqlite3_vec_init as *const ())));
    });
}

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
    sqlite_accepts(&out);
}

#[test]
fn double_colon_int_literal_cast_uses_cast_syntax() {
    let out = tr("SELECT '1'::int");
    assert!(out.contains("CAST('1' AS INTEGER)"), "{out}");
    assert!(!out.contains("::"), "cast operator leaked into output: {out}");
    sqlite_accepts(&out);
}

/// A NUMERIC column is emitted as an INTEGER of minor units, so a cast to
/// NUMERIC has to move the point rather than change a storage class. `val` here
/// belongs to no declared table, so its scale is unknown and shifting it either
/// way would be a guess. The value cases live in
/// `tests/test_numeric_scaled_integer.rs`, where the columns have types.
#[test]
fn double_colon_numeric_cast_needs_a_resolvable_scale() {
    let error = translate_pg("SELECT val::numeric(10, 2) FROM t", &Pg2SqliteOptions::default())
        .expect_err("the operand's scale cannot be resolved");
    assert!(error.to_string().contains("scale"), "got: {error}");
}

#[test]
fn nested_double_colon_casts_have_no_operator() {
    let out = tr("SELECT (a::int)::text FROM t");
    assert!(!out.contains("::"), "nested cast operator leaked: {out}");
    assert!(out.matches("CAST(").count() >= 2, "expected two CAST calls: {out}");
    sqlite_accepts(&out);
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
    sqlite_syntax_check(&out);
}

#[test]
fn uuid_blob_cast_still_lowers_to_conversion() {
    // Regression: `::uuid` under the Blob representation is rewritten by its
    // own branch, not the generic CAST path.
    let opts = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7");
    let out = translate_pg("SELECT '11111111-1111-1111-1111-111111111111'::uuid FROM t", &opts)
        .expect("translation failed")
        .join("\n");
    assert!(!out.contains("::"), "{out}");
    assert!(
        !out.to_uppercase().contains("AS UUID"),
        "uuid cast fell through to generic path: {out}"
    );
    sqlite_accepts(&out);
}

/// SQLite has no cast format clause. Cloning `FORMAT` through emitted
/// `CAST(x AS TEXT FORMAT '...')`, which SQLite rejects at parse time, so the
/// translator has to refuse the cast instead.
#[test]
fn cast_with_a_format_clause_is_rejected() {
    let err =
        translate_pg("SELECT CAST(a AS TEXT FORMAT 'YYYY') FROM t", &Pg2SqliteOptions::default())
            .expect_err("a cast format clause should be rejected");
    let msg = err.to_string();
    assert!(msg.contains("cast format"), "error should name the clause, got: {msg}");
}

/// An array target type is not made valid by the `CAST` spelling: it needs the
/// array representation like any other array construct.
#[test]
fn array_cast_target_needs_an_array_representation() {
    for pg in ["SELECT a::int[] FROM t", "SELECT CAST(a AS INT ARRAY) FROM t"] {
        let err = translate_pg(pg, &Pg2SqliteOptions::default())
            .expect_err("an array cast target should be rejected");
        assert!(
            err.to_string().contains("with_array_representation"),
            "error should name the opt-in for {pg}, got: {err}"
        );
    }
}

/// Under the JSON representation an array cast collapses to `CAST(x AS TEXT)`,
/// matching the column type the array maps to.
#[test]
fn array_cast_target_becomes_text_under_json_arrays() {
    let opts = Pg2SqliteOptions::default()
        .with_array_representation(pg2sqlite::prelude::ArrayRepresentation::Json);
    let out = translate_pg("SELECT a::int[] FROM t", &opts)
        .expect("array cast should translate")
        .join("\n");
    assert!(out.contains("CAST(a AS TEXT)"), "{out}");
    sqlite_accepts(&out);
}

/// Execute the emitted SQL against an in-memory SQLite to prove it is accepted.
/// Called from tests that produce standard SQLite output with no extension
/// functions.
fn sqlite_accepts(sql: &str) {
    let mut conn = establish_connection();
    conn.batch_execute("CREATE TABLE t (id INTEGER, a TEXT, n INTEGER) STRICT;").unwrap();
    conn.batch_execute(&format!("{sql};"))
        .unwrap_or_else(|e| panic!("emitted SQL rejected by SQLite: {e}\n{sql}"));
}

/// Execute emitted SQL that references sqlite-vec extension functions.
///
/// Registers sqlite-vec globally so the functions are available, then runs the
/// statement on a fresh in-memory connection. rusqlite is used directly
/// because diesel does not expose `sqlite3_auto_extension`.
fn sqlite_syntax_check(sql: &str) {
    register_sqlite_vec_once();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE t (id INTEGER, a TEXT, embedding BLOB);").unwrap();
    conn.execute_batch(&format!("{sql};"))
        .unwrap_or_else(|e| panic!("SQLite rejected emitted SQL: {e}\n{sql}"));
}
