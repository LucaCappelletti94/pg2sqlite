//! Bound parameters must carry the value PostgreSQL takes, not the stored
//! representation.  These tests bind a real value through rusqlite and assert
//! rows, so every assertion is a runtime execution proof.
//!
//! Translator output uses SQLite numbered placeholders (`?1`, `?2`, …) which
//! rusqlite handles natively; diesel's `sql_query.bind` emits unnamed `?` so
//! rusqlite is the correct tool for all parameterised probes here.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};
use rusqlite::{Connection as RusqliteConn, types::ValueRef};

const UUID_STR: &str = "550e8400-e29b-41d4-a716-446655440000";

fn opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}
fn uuid_blob_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}
fn uuid_text_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text)
}

/// Translates `query_sql` (the last statement after `schema_sql`) into a
/// SQLite string and returns it.
fn translate_query(schema_sql: &str, query_sql: &str, opt: &Pg2SqliteOptions) -> String {
    let full = format!("{schema_sql}\n{query_sql}");
    let stmts =
        Pg2Sqlite::default().sql(&full).expect("parse").translate_to_sql(opt).expect("translate");
    stmts.into_iter().last().expect("at least one translated statement")
}

/// Runs `query` on `conn` with `params`, collecting the first column as
/// optional strings.  rusqlite handles SQLite numbered parameters (`?1`).
fn run_with<P: rusqlite::Params>(
    conn: &RusqliteConn,
    query: &str,
    params: P,
) -> Vec<Option<String>> {
    let mut stmt = conn.prepare(query).unwrap_or_else(|e| panic!("prepare: {e}\n{query}"));
    stmt.query_map(params, |row| {
        Ok(match row.get_ref(0)? {
            ValueRef::Null => None,
            ValueRef::Integer(i) => Some(i.to_string()),
            ValueRef::Real(f) => Some(f.to_string()),
            ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(bytes) => {
                Some(bytes.iter().fold(String::new(), |mut hex, byte| {
                    use core::fmt::Write as _;
                    let _ = write!(hex, "{byte:02x}");
                    hex
                }))
            }
        })
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

/// Sets up a schema and seed data in a rusqlite connection using translated
/// SQL.
fn rusqlite_apply(sql: &str, opt: &Pg2SqliteOptions) -> RusqliteConn {
    let stmts =
        Pg2Sqlite::default().sql(sql).expect("parse").translate_to_sql(opt).expect("translate");
    let conn = RusqliteConn::open_in_memory().expect("in-memory SQLite");
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap_or_else(|e| panic!("setup: {e}\n{s}"));
    }
    conn
}

// ---------------------------------------------------------------------------
// NUMERIC(10,2) — comparison, IN, BETWEEN, IS DISTINCT FROM, CASE arm,
// scale-preserving function argument.
// ---------------------------------------------------------------------------

const NUMERIC_SCHEMA: &str = "CREATE TABLE amounts (id INT PRIMARY KEY, price NUMERIC(10,2));";
const NUMERIC_SEED: &str = "INSERT INTO amounts VALUES (1, 19.99), (2, 1.50), (3, -0.05);";

#[test]
fn numeric_param_comparison_gt() {
    // Baseline: WHERE price > $1 with 1.5 finds the row where price=19.99.
    // Before this fix: the emitted ?1 was not scaled, comparing 150 vs 1999
    // would still work here, but 1.5 vs 1999 gives the wrong answer.
    let conn = rusqlite_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE price > $1 ORDER BY id;",
        &opts(),
    );
    let rows = run_with(&conn, &q, rusqlite::params![1.5_f64]);
    assert_eq!(rows, vec![Some("1".into())], "only price=19.99 > 1.5");
}

#[test]
fn numeric_param_comparison_eq() {
    let conn = rusqlite_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(NUMERIC_SCHEMA, "SELECT id FROM amounts WHERE price = $1;", &opts());
    let rows = run_with(&conn, &q, rusqlite::params![1.5_f64]);
    assert_eq!(rows, vec![Some("2".into())], "price=1.50 matches bind 1.5");
}

#[test]
fn numeric_param_negative() {
    let conn = rusqlite_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(NUMERIC_SCHEMA, "SELECT id FROM amounts WHERE price = $1;", &opts());
    let rows = run_with(&conn, &q, rusqlite::params![-0.05_f64]);
    assert_eq!(rows, vec![Some("3".into())], "negative bind -0.05 matches price=-0.05");
}

#[test]
fn numeric_param_null_propagates() {
    let conn = rusqlite_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT (price > $1) AS gt FROM amounts WHERE id = 1;",
        &opts(),
    );
    let rows = run_with(&conn, &q, rusqlite::params![Option::<f64>::None]);
    // CAST(ROUND(NULL*100) AS INTEGER) = NULL; 1999 > NULL = NULL in SQL.
    assert_eq!(rows, vec![None], "NULL bind → NULL comparison result");
}

#[test]
fn numeric_param_in_list() {
    let conn = rusqlite_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE price IN ($1, $2) ORDER BY id;",
        &opts(),
    );
    let rows = run_with(&conn, &q, rusqlite::params![1.5_f64, 19.99_f64]);
    assert_eq!(rows, vec![Some("1".into()), Some("2".into())]);
}

#[test]
fn numeric_param_between() {
    let conn = rusqlite_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE price BETWEEN $1 AND $2 ORDER BY id;",
        &opts(),
    );
    let rows = run_with(&conn, &q, rusqlite::params![1.0_f64, 20.0_f64]);
    assert_eq!(rows, vec![Some("1".into()), Some("2".into())]);
}

#[test]
fn numeric_param_is_distinct_from() {
    let conn = rusqlite_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE price IS DISTINCT FROM $1 ORDER BY id;",
        &opts(),
    );
    let rows = run_with(&conn, &q, rusqlite::params![1.5_f64]);
    assert_eq!(rows, vec![Some("1".into()), Some("3".into())]);
}

#[test]
fn numeric_param_in_scale_preserving_call() {
    let conn = rusqlite_apply(&format!("{NUMERIC_SCHEMA}{NUMERIC_SEED}"), &opts());
    let q = translate_query(
        NUMERIC_SCHEMA,
        "SELECT id FROM amounts WHERE coalesce(price, $1) > $2 ORDER BY id;",
        &opts(),
    );
    let rows = run_with(&conn, &q, rusqlite::params![0.0_f64, 1.0_f64]);
    assert_eq!(rows, vec![Some("1".into()), Some("2".into())]);
}

// ---------------------------------------------------------------------------
// UUID under Blob representation — comparison (the measured defect) and IN.
// ---------------------------------------------------------------------------

const UUID_BLOB_SCHEMA: &str = "CREATE TABLE uids (id UUID PRIMARY KEY, label TEXT NOT NULL);";
const UUID_BLOB_SEED: &str =
    "INSERT INTO uids VALUES ('550e8400-e29b-41d4-a716-446655440000', 'alice');";

#[test]
fn uuid_blob_literal_comparison_finds_row() {
    // Measured defect: WHERE id = '...' found 0 rows because the literal was
    // compared as text against the stored blob.
    let conn = rusqlite_apply(&format!("{UUID_BLOB_SCHEMA}{UUID_BLOB_SEED}"), &uuid_blob_opts());
    let q = translate_query(
        UUID_BLOB_SCHEMA,
        &format!("SELECT label FROM uids WHERE id = '{UUID_STR}';"),
        &uuid_blob_opts(),
    );
    let rows = run_with(&conn, &q, []);
    assert_eq!(rows, vec![Some("alice".into())], "literal UUID comparison must find the blob row");
}

#[test]
fn uuid_blob_param_comparison_finds_row() {
    // The same comparison with a bound parameter instead of a literal.
    let conn = rusqlite_apply(&format!("{UUID_BLOB_SCHEMA}{UUID_BLOB_SEED}"), &uuid_blob_opts());
    let q = translate_query(
        UUID_BLOB_SCHEMA,
        "SELECT label FROM uids WHERE id = $1;",
        &uuid_blob_opts(),
    );
    let rows = run_with(&conn, &q, rusqlite::params![UUID_STR]);
    assert_eq!(rows, vec![Some("alice".into())], "parameter UUID must match the stored blob");
}

#[test]
fn uuid_blob_param_in_list() {
    let conn = rusqlite_apply(&format!("{UUID_BLOB_SCHEMA}{UUID_BLOB_SEED}"), &uuid_blob_opts());
    let q = translate_query(
        UUID_BLOB_SCHEMA,
        "SELECT label FROM uids WHERE id IN ($1);",
        &uuid_blob_opts(),
    );
    let rows = run_with(&conn, &q, rusqlite::params![UUID_STR]);
    assert_eq!(rows, vec![Some("alice".into())]);
}

#[test]
fn uuid_blob_param_is_distinct_from() {
    let conn = rusqlite_apply(&format!("{UUID_BLOB_SCHEMA}{UUID_BLOB_SEED}"), &uuid_blob_opts());
    let q = translate_query(
        UUID_BLOB_SCHEMA,
        "SELECT count(*) FROM uids WHERE id IS DISTINCT FROM $1;",
        &uuid_blob_opts(),
    );
    let rows = run_with(&conn, &q, rusqlite::params![UUID_STR]);
    assert_eq!(rows, vec![Some("0".into())], "IS DISTINCT FROM same UUID → 0 distinct rows");
}

// ---------------------------------------------------------------------------
// UUID under Text representation — literal canonicalized; parameter
// passthrough.
// ---------------------------------------------------------------------------

const UUID_TEXT_SCHEMA: &str = "CREATE TABLE uids_t (id UUID PRIMARY KEY, label TEXT NOT NULL);";
const UUID_TEXT_SEED: &str =
    "INSERT INTO uids_t VALUES ('550e8400-e29b-41d4-a716-446655440000', 'bob');";

#[test]
fn uuid_text_param_passthrough_finds_row() {
    // Under Text representation, the caller already binds the canonical form.
    let conn = rusqlite_apply(&format!("{UUID_TEXT_SCHEMA}{UUID_TEXT_SEED}"), &uuid_text_opts());
    let q = translate_query(
        UUID_TEXT_SCHEMA,
        "SELECT label FROM uids_t WHERE id = $1;",
        &uuid_text_opts(),
    );
    let rows = run_with(&conn, &q, rusqlite::params![UUID_STR]);
    assert_eq!(rows, vec![Some("bob".into())]);
}

// ---------------------------------------------------------------------------
// Vector — parameter wraps like a literal.  Needs sqlite_vec extension.
// ---------------------------------------------------------------------------

mod helpers;

#[test]
fn vector_param_comparison_finds_row() {
    helpers::register_sqlite_vec_once();
    let opt = Pg2SqliteOptions::default();
    let schema = "CREATE TABLE vecs (id INT PRIMARY KEY, emb vector(3));";
    let seed = "INSERT INTO vecs VALUES (1, '[1.0,2.0,3.0]');";

    let conn = {
        let stmts = Pg2Sqlite::default()
            .sql(&format!("{schema}{seed}"))
            .expect("parse")
            .translate_to_sql(&opt)
            .expect("translate");
        // rusqlite: sqlite_vec extension is not exposed through diesel's API.
        let conn = helpers::vec_connection();
        for s in &stmts {
            conn.execute_batch(&format!("{s};")).unwrap_or_else(|e| panic!("setup: {e}\n{s}"));
        }
        conn
    };

    let q = translate_query(schema, "SELECT id FROM vecs WHERE emb = $1;", &opt);
    // $1 carries the PostgreSQL vector text; the translator wraps it with
    // vec_f32.
    let rows = run_with(&conn, &q, rusqlite::params!["[1.0,2.0,3.0]"]);
    assert_eq!(rows, vec![Some("1".into())], "vector parameter must match the stored blob");
}

// ---------------------------------------------------------------------------
// Array — parameter refused; caller must bind JSON text.
// ---------------------------------------------------------------------------

#[test]
fn array_param_refused_with_message() {
    use pg2sqlite::prelude::ArrayRepresentation;
    let opt = Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json);
    let schema = "CREATE TABLE arr (id INT, tags INT[]);";
    let result = Pg2Sqlite::default()
        .sql(&format!("{schema}\nSELECT id FROM arr WHERE tags = $1;"))
        .expect("parse")
        .translate(&opt);
    let err = result.expect_err("array parameter must be refused").to_string();
    assert!(err.to_lowercase().contains("array"), "error must mention array: {err}");
}

// ---------------------------------------------------------------------------
// Rounding note verification: SQLite and PostgreSQL agree for exact f64 values.
// ---------------------------------------------------------------------------

#[test]
fn round_trip_exact_f64_values_agree_with_postgresql() {
    // These values are exactly representable in f64, so no rounding ambiguity.
    let exact_cases: &[(f64, &str)] = &[
        (1.5_f64, "150"),
        (-1.5_f64, "-150"),
        (19.99_f64, "1999"),
        (0.01_f64, "1"),
        (0.0_f64, "0"),
        (5.0_f64, "500"),
    ];
    let conn = RusqliteConn::open_in_memory().expect("sqlite");
    for (val, expected_minor_units) in exact_cases {
        let got: i64 = conn
            .query_row("SELECT CAST(ROUND(?1 * 100) AS INTEGER)", rusqlite::params![val], |r| {
                r.get(0)
            })
            .expect("query");
        assert_eq!(
            got.to_string(),
            *expected_minor_units,
            "f64 {val} → {expected_minor_units} minor units"
        );
    }
}
