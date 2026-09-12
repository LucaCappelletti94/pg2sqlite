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

// ── RANGE frame bounds that are not scaled (shared_helpers.rs other arm) ─────

/// CURRENT ROW hits the `other` arm in translate_window_frame_bound — no
/// scaling is applied. PostgreSQL: running sum from current row to end.
/// Measured: docker postgres:17-alpine.
#[test]
fn range_frame_current_row_bound_passthrough() {
    let (query, mut conn) = setup_and_query(
        "
        CREATE TABLE rnge (id INTEGER PRIMARY KEY, amount NUMERIC(10,2) NOT NULL);
        INSERT INTO rnge VALUES (1,1.00),(2,2.00),(3,3.00);
        SELECT id,
               sum(id) OVER (ORDER BY amount RANGE BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING)
               AS total
        FROM rnge ORDER BY id;
    ",
    );
    // CURRENT ROW and UNBOUNDED FOLLOWING both hit the `other` arm (no literal
    // to scale). Window function requires diesel::sql_query.
    let rows = diesel::sql_query(&query).load::<WindowRow>(&mut conn).unwrap();
    // id=1 (amount 1.00): rows from 1.00 forward → 1+2+3=6
    // id=2 (amount 2.00): rows from 2.00 forward → 2+3=5
    // id=3 (amount 3.00): rows from 3.00 forward → 3
    assert_eq!(rows, [(1, 6), (2, 5), (3, 3)].map(|(id, total)| WindowRow { id, total }).to_vec());
}

/// UNBOUNDED PRECEDING also hits the `other` arm; running sum up to current
/// row. Measured: docker postgres:17-alpine.
#[test]
fn range_frame_unbounded_preceding_passthrough() {
    let (query, mut conn) = setup_and_query(
        "
        CREATE TABLE rnge2 (id INTEGER PRIMARY KEY, amount NUMERIC(10,2) NOT NULL);
        INSERT INTO rnge2 VALUES (1,1.00),(2,2.00),(3,3.00);
        SELECT id,
               sum(id) OVER (ORDER BY amount RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)
               AS total
        FROM rnge2 ORDER BY id;
    ",
    );
    // Window function requires diesel::sql_query.
    let rows = diesel::sql_query(&query).load::<WindowRow>(&mut conn).unwrap();
    // Running sum: 1, 1+2=3, 1+2+3=6.
    assert_eq!(rows, [(1, 1), (2, 3), (3, 6)].map(|(id, total)| WindowRow { id, total }).to_vec());
}

/// A RANGE frame over an INTEGER ORDER BY key has no NUMERIC scale, so the
/// bound passes through unscaled (range_numeric_scale = None → line 2024 early
/// return). Measured: docker postgres:17-alpine.
#[test]
fn range_frame_on_integer_key_has_no_scale_applied() {
    let (query, mut conn) = setup_and_query(
        "
        CREATE TABLE scores (id INTEGER PRIMARY KEY, score INTEGER NOT NULL);
        INSERT INTO scores VALUES (1,1),(2,2),(3,10);
        SELECT id,
               sum(id) OVER (ORDER BY score RANGE BETWEEN 1 PRECEDING AND 1 FOLLOWING) AS total
        FROM scores ORDER BY id;
    ",
    );
    // Window function requires diesel::sql_query.
    let rows = diesel::sql_query(&query).load::<WindowRow>(&mut conn).unwrap();
    // score 1 and 2 are within 1 of each other; score 10 is alone → sum=3 each.
    assert_eq!(rows, [(1, 3), (2, 3), (3, 3)].map(|(id, total)| WindowRow { id, total }).to_vec());
}

// ── DISTINCT ON: ORDER BY shorter than DISTINCT ON list (query.rs line 521) ──

/// When `ORDER BY` has fewer terms than `DISTINCT ON`, PostgreSQL rejects it;
/// pg2sqlite must refuse it too.
#[test]
fn distinct_on_refuses_when_order_by_shorter_than_partition_list() {
    let err = translate_err(
        "CREATE TABLE t2 (a TEXT, b TEXT, c TEXT);
         SELECT DISTINCT ON (a, b) a, b, c FROM t2 ORDER BY a;",
    );
    assert!(
        err.contains("DISTINCT ON expressions must match initial ORDER BY expressions"),
        "expected prefix-check refusal, got: {err}"
    );
}

// ── DISTINCT ON: no ORDER BY at all (query.rs line 514 None arm) ─────────────

#[derive(QueryableByName, Debug)]
struct GrpOnly {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    grp: Option<String>,
}

/// PostgreSQL accepts `DISTINCT ON (x)` without ORDER BY; pg2sqlite must also
/// accept it and produce one row per partition group. The row picked per group
/// is arbitrary, so only the count is asserted.
#[test]
fn distinct_on_without_order_by_runs() {
    let (query, mut conn) = setup_and_query(
        "
        CREATE TABLE unordered (id INTEGER PRIMARY KEY, grp TEXT);
        INSERT INTO unordered VALUES (1,'a'),(2,'a'),(3,'b');
        SELECT DISTINCT ON (grp) grp FROM unordered;
    ",
    );
    // DISTINCT ON rewrite uses ROW_NUMBER; diesel::sql_query required.
    let rows = diesel::sql_query(&query).load::<GrpOnly>(&mut conn).unwrap();
    let grps: Vec<Option<&str>> = rows.iter().map(|r| r.grp.as_deref()).collect();
    assert_eq!(grps.len(), 2, "two distinct groups expected");
    assert!(grps.contains(&Some("a")), "group 'a' must appear: {grps:?}");
    assert!(grps.contains(&Some("b")), "group 'b' must appear: {grps:?}");
}

// ── DISTINCT ON: ORDER BY already names the output alias (query.rs 616-619) ──

/// When the outer ORDER BY already uses the projected alias (`grp AS g`
/// ordered `BY g`), `alias_for_expr_in_projection` hits the
/// alias-equality branch at line 616-619 rather than the expression-equality
/// branch at line 612.
#[test]
fn distinct_on_order_by_alias_name_runs() {
    let (query, mut conn) = setup_and_query(
        "
        CREATE TABLE aliased (id INTEGER PRIMARY KEY, grp TEXT, lbl TEXT);
        INSERT INTO aliased VALUES (1,'a','z'),(2,'a','m'),(3,'b','p');
        SELECT DISTINCT ON (grp) grp AS g, lbl FROM aliased ORDER BY g, lbl;
    ",
    );
    // Window function rewrite; diesel::sql_query required.
    let rows = diesel::sql_query(&query).load::<CategoryRow>(&mut conn).unwrap();
    assert_eq!(rows.len(), 2, "two distinct groups expected");
    // grp 'a' → first by lbl = 'm'; grp 'b' → 'p'.
    assert!(rows.iter().any(|r| r.g.as_deref() == Some("a") && r.lbl == "m"));
    assert!(rows.iter().any(|r| r.g.as_deref() == Some("b") && r.lbl == "p"));
}
