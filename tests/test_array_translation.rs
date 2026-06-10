//! Red tests for PostgreSQL array translation, backed by SQLite json1.
//!
//! Design and rationale live in
//! `docs/future_work_translation_coverage.md`. The plan stores a PG
//! array as a TEXT column holding a JSON array, and rewrites every array
//! operation into a json1 expression over that text. json1 is built into
//! SQLite, so there is no extension to ship.
//!
//! These tests are written test-first and START red. Each phase is
//! greenified independently as its slice of the mapping lands:
//!
//!   Phase 1 (storage + construction): column type `T[]` to TEXT, array
//!     literals to `json_array(...)`, schema-aware literal wrapping.
//!   Phase 2 (read): `array_length`/`cardinality`, subscript, `= ANY`,
//!     `array_to_string`.
//!   Phase 3 (aggregate + set-returning): `array_agg` to
//!     `json_group_array`, `unnest` to `json_each`.
//!   Phase 4 (operators): `||` concatenation. Containment operators
//!     (`@>`, `<@`, `&&`) are deferred and may need sqlparser-fork work
//!     to even parse, so they are not pinned here yet.
//!
//! As each function arm currently returns `UnsupportedSQLiteFeature` (or
//! passes the construct through unchanged), the substring assertions
//! below fail until the json1 rewrite is implemented. The
//! `multidim_array_stays_unsupported` guard is the exception: it is GREEN
//! today and must STAY green, since multidimensional arrays have no clean
//! nested-JSON mapping and should keep erroring even after 1-D support.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// Translate and join to a single SQL script. Panics on translation
/// failure, which is the red signal while the feature is unimplemented.
fn translate(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .and_then(|t| t.translate(&Pg2SqliteOptions::default()))
        .map(|stmts| stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|e| panic!("translation failed (array support not implemented yet?): {e}"))
}

fn try_translate(sql: &str) -> Result<String, pg2sqlite::errors::Error> {
    Pg2Sqlite::default()
        .sql(sql)
        .and_then(|t| t.translate(&Pg2SqliteOptions::default()))
        .map(|stmts| stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
}

// -----------------------------------------------------------------------
// Phase 1: storage + construction
// -----------------------------------------------------------------------

#[test]
fn p1_text_array_column_maps_to_text() {
    let out = translate("CREATE TABLE posts (id INTEGER PRIMARY KEY, tags TEXT[]);");
    assert!(out.contains("tags TEXT"), "text[] column should store as TEXT, got:\n{out}");
}

#[test]
fn p1_int_array_column_maps_to_text() {
    let out = translate("CREATE TABLE m (id INTEGER PRIMARY KEY, xs INTEGER[]);");
    assert!(out.contains("xs TEXT"), "integer[] column should store as TEXT, got:\n{out}");
}

#[test]
fn p1_array_constructor_maps_to_json_array() {
    let out = translate("SELECT ARRAY[1, 2, 3] AS xs;");
    assert!(
        out.contains("json_array(1, 2, 3)"),
        "ARRAY[...] should become json_array(...), got:\n{out}"
    );
}

#[test]
fn p1_curly_array_literal_becomes_json_text() {
    // Schema-aware: the translator knows `tags` is an array column and
    // rewrites the curly literal into a JSON array text.
    let out = translate(
        "CREATE TABLE posts (id INTEGER PRIMARY KEY, tags TEXT[]);\n\
         INSERT INTO posts (id, tags) VALUES (1, '{a,b,c}');",
    );
    assert!(
        out.contains("[\"a\",\"b\",\"c\"]"),
        "curly array literal should become a JSON array text, got:\n{out}"
    );
}

#[test]
fn p1_array_roundtrips_through_in_memory_sqlite() {
    use rusqlite::Connection;
    // Translate schema + insert together so the literal wrap sees the
    // array column type, then apply and read the element count back via
    // json1 to prove the stored representation is a real JSON array.
    let script = translate(
        "CREATE TABLE posts (id INTEGER PRIMARY KEY, tags TEXT[]);\n\
         INSERT INTO posts (id, tags) VALUES (1, ARRAY['x', 'y', 'z']);",
    );
    let conn = Connection::open_in_memory().expect("open sqlite");
    conn.execute_batch(&script).expect("apply array schema + insert");
    let n: i64 = conn
        .query_row("SELECT json_array_length(tags) FROM posts WHERE id = 1", [], |r| r.get(0))
        .expect("query json_array_length");
    assert_eq!(n, 3, "stored array should hold 3 elements");
}

// -----------------------------------------------------------------------
// Phase 2: read (length, subscript, membership, join-to-string)
// -----------------------------------------------------------------------

#[test]
fn p2_array_length_maps_to_json_array_length() {
    let out = translate("SELECT array_length(ARRAY[1, 2, 3], 1) AS n;");
    assert!(
        out.contains("json_array_length"),
        "array_length should map to json_array_length, got:\n{out}"
    );
}

#[test]
fn p2_cardinality_maps_to_json_array_length() {
    let out = translate("SELECT cardinality(ARRAY[1, 2, 3]) AS n;");
    assert!(
        out.contains("json_array_length"),
        "cardinality should map to json_array_length, got:\n{out}"
    );
}

#[test]
fn p2_subscript_is_one_based() {
    // PG subscripts are 1-based; json1 paths are 0-based, so arr[1] is $[0].
    let out = translate("SELECT (ARRAY[10, 20, 30])[1] AS first;");
    assert!(
        out.contains("json_extract") && out.contains("$[0]"),
        "arr[1] should become json_extract(..., '$[0]'), got:\n{out}"
    );
}

#[test]
fn p2_any_membership_maps_to_json_each() {
    let out = translate("SELECT 2 = ANY(ARRAY[1, 2, 3]) AS hit;");
    assert!(
        out.contains("json_each"),
        "= ANY(arr) should become IN (SELECT value FROM json_each(arr)), got:\n{out}"
    );
}

#[test]
fn p2_array_to_string_maps_to_group_concat() {
    let out = translate("SELECT array_to_string(ARRAY['a', 'b'], ',') AS joined;");
    assert!(
        out.contains("group_concat") && out.contains("json_each"),
        "array_to_string should group_concat over json_each, got:\n{out}"
    );
}

// -----------------------------------------------------------------------
// Phase 3: aggregate + set-returning
// -----------------------------------------------------------------------

#[test]
fn p3_array_agg_maps_to_json_group_array() {
    let out = translate("SELECT array_agg(v) AS xs FROM t;");
    assert!(
        out.contains("json_group_array"),
        "array_agg should map to json_group_array, got:\n{out}"
    );
}

#[test]
fn p3_unnest_in_from_maps_to_json_each() {
    let out = translate("SELECT * FROM unnest(ARRAY[1, 2, 3]) AS x;");
    assert!(out.contains("json_each"), "unnest in FROM should become json_each, got:\n{out}");
}

// -----------------------------------------------------------------------
// Phase 4: operators
// -----------------------------------------------------------------------

#[test]
fn p4_array_concat_maps_to_json() {
    let out = translate("SELECT ARRAY[1, 2] || ARRAY[3, 4] AS xs;");
    assert!(
        out.contains("json_each") || out.contains("json_group_array"),
        "array || array should build a JSON array via json_each, got:\n{out}"
    );
}

// -----------------------------------------------------------------------
// Boundary guard (already green, must stay green)
//
// Multidimensional arrays have no clean nested-JSON mapping (PG indexes
// them as arr[i][j] over a rectangular array, which is not the same as a
// JSON array of JSON arrays), so they must keep erroring even after 1-D
// support lands.
// -----------------------------------------------------------------------

#[test]
fn multidim_array_stays_unsupported() {
    let result = try_translate("CREATE TABLE m (id INTEGER PRIMARY KEY, grid INTEGER[][]);");
    assert!(result.is_err(), "multidimensional arrays must stay unsupported, got:\n{result:?}");
}
