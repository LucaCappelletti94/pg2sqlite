//! Tests for the five AggregateWindow scout findings.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[path = "helpers/translate.rs"]
mod translate_helpers;
use translate_helpers::translate_default_err as translate_err;

fn translate_to_stmts(pg: &str) -> Vec<String> {
    Pg2Sqlite::default().sql(pg).unwrap().translate_to_sql(&Pg2SqliteOptions::default()).unwrap()
}

fn setup_and_query(pg: &str) -> (String, SqliteConnection) {
    let mut stmts = translate_to_stmts(pg);
    let query = stmts.pop().unwrap();
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    for stmt in &stmts {
        // Translated DDL/DML cannot be expressed through Diesel's typed DSL.
        diesel::sql_query(stmt.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("setup failed: {e}\n{stmt}"));
    }
    (query, conn)
}

// ── Finding 1: RANGE frame bound scaled for NUMERIC ORDER BY ─────────────────

#[derive(QueryableByName, Clone, Debug, PartialEq)]
struct WindowRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    // Window function result; diesel::sql_query required (no OVER in the typed DSL).
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    total: i64,
}

/// RANGE 1 on a NUMERIC(10,2) ORDER BY must be scaled to 100 minor units.
/// Measured: docker postgres:17-alpine.
#[test]
fn range_frame_on_numeric_order_by_scales_bound() {
    let (query, mut conn) = setup_and_query(
        "
        CREATE TABLE ledger (id INTEGER PRIMARY KEY, amount NUMERIC(10,2) NOT NULL);
        INSERT INTO ledger VALUES (1,1.00),(2,2.00),(3,3.00),(4,10.00),(5,20.00);
        SELECT id,
               sum(id) OVER (ORDER BY amount RANGE BETWEEN 1 PRECEDING AND 1 FOLLOWING) AS total
        FROM ledger ORDER BY id;
    ",
    );
    // Window function requires diesel::sql_query.
    let rows = diesel::sql_query(&query).load::<WindowRow>(&mut conn).unwrap();
    // PostgreSQL: id=1→3, 2→6, 3→5, 4→4, 5→5.
    assert_eq!(
        rows,
        [(1, 3), (2, 6), (3, 5), (4, 4), (5, 5)]
            .map(|(id, total)| WindowRow { id, total })
            .to_vec()
    );
}

/// ROWS counts row positions; the ORDER BY key's scale is irrelevant.
#[test]
fn rows_frame_on_numeric_order_by_unchanged() {
    let (query, mut conn) = setup_and_query(
        "
        CREATE TABLE ledger (id INTEGER PRIMARY KEY, amount NUMERIC(10,2) NOT NULL);
        INSERT INTO ledger VALUES (1,1.00),(2,2.00),(3,3.00),(4,10.00),(5,20.00);
        SELECT id,
               sum(id) OVER (ORDER BY amount ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) AS total
        FROM ledger ORDER BY id;
    ",
    );
    let rows = diesel::sql_query(&query).load::<WindowRow>(&mut conn).unwrap();
    // PostgreSQL: ROWS is position-based; scale does not apply.
    assert_eq!(
        rows,
        [(1, 3), (2, 6), (3, 9), (4, 12), (5, 9)]
            .map(|(id, total)| WindowRow { id, total })
            .to_vec()
    );
}

// ── Finding 2: jsonb_object_agg refused ──────────────────────────────────────

#[test]
fn jsonb_object_agg_is_refused() {
    let err =
        translate_err("CREATE TABLE t (k TEXT, v INTEGER); SELECT jsonb_object_agg(k, v) FROM t;");
    assert!(err.contains("jsonb_object_agg"), "refusal must name the function: {err}");
}

// ── Finding 3: DISTINCT ON outer ORDER BY uses projected aliases ─────────────

#[derive(QueryableByName, Debug)]
struct CategoryRow {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    g: Option<String>,
    // DISTINCT ON rewrite uses ROW_NUMBER; diesel::sql_query required.
    #[diesel(sql_type = diesel::sql_types::Text)]
    lbl: String,
}

/// PostgreSQL: DISTINCT ON (grp) ORDER BY grp, label → a→alpha, b→delta,
/// NULL→epsilon. Measured: docker postgres:17-alpine.
#[test]
fn distinct_on_aliased_partition_column_runs() {
    let (query, mut conn) = setup_and_query(
        "
        CREATE TABLE categories (id INTEGER PRIMARY KEY, grp TEXT, label TEXT NOT NULL);
        INSERT INTO categories VALUES (1,'a','alpha'),(2,'a','beta'),
                                      (3,'b','gamma'),(4,'b','delta'),(5,NULL,'epsilon');
        SELECT DISTINCT ON (grp) grp AS g, label AS lbl
        FROM categories ORDER BY grp, label;
    ",
    );
    let rows = diesel::sql_query(&query).load::<CategoryRow>(&mut conn).unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(|r| r.g.as_deref() == Some("a") && r.lbl == "alpha"));
    assert!(rows.iter().any(|r| r.g.as_deref() == Some("b") && r.lbl == "delta"));
    assert!(rows.iter().any(|r| r.g.is_none() && r.lbl == "epsilon"));
}

// ── Finding 4: DISTINCT ON validates ORDER BY prefix ─────────────────────────

#[test]
fn distinct_on_refuses_when_order_by_does_not_match() {
    let err = translate_err(
        "CREATE TABLE categories (id INTEGER PRIMARY KEY, grp TEXT, label TEXT NOT NULL);
         SELECT DISTINCT ON (grp) grp AS g, label AS lbl FROM categories ORDER BY label;",
    );
    assert!(
        err.contains("DISTINCT ON expressions must match initial ORDER BY expressions"),
        "expected PostgreSQL-worded refusal, got: {err}"
    );
}

// ── Finding 5: bool_and/bool_or advice is NULL-safe ──────────────────────────

#[test]
fn bool_and_refusal_advice_is_null_safe() {
    let err = translate_err("CREATE TABLE t (x BOOLEAN); SELECT bool_and(x) FROM t;");
    assert!(err.contains("bool_and"), "refusal must name the function: {err}");
    // ELSE 0 turns NULL→0, collapsing NULL inputs to false rather than skipping
    // them.
    assert!(!err.contains("ELSE 0"), "advice must not use ELSE 0: {err}");
    assert!(err.contains("WHEN NOT"), "advice must use WHEN NOT for the false branch: {err}");
}

#[test]
fn bool_or_refusal_advice_is_null_safe() {
    let err = translate_err("CREATE TABLE t (x BOOLEAN); SELECT bool_or(x) FROM t;");
    assert!(err.contains("bool_or"), "refusal must name the function: {err}");
    assert!(!err.contains("ELSE 0"), "advice must not use ELSE 0: {err}");
    assert!(err.contains("WHEN NOT"), "advice must use WHEN NOT for the false branch: {err}");
}
