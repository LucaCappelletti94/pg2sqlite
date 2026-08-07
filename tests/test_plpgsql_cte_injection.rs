//! Tests for PL/pgSQL inject_with_into_in_subqueries missing arms (GROUP G).
//!
//! G1: Case, InList, Between, Cast, Function, Tuple arms missing from
//! catch-all. When a CTE-referencing subquery is nested inside one of these
//! expressions, the WITH clause must be injected into the subquery.
//!
//! The three R125 pins flipped here: `SELECT ... INTO var` with no FROM
//! clause used to survive verbatim into the emitted trigger body, which
//! SQLite refuses with `near "INTO": syntax error`. Each test now consumes
//! the bound variable in a second statement, so the CTE injection under test
//! still materialises in the output, and the trigger is exercised by
//! execution with its effect asserted row by row.

mod helpers;

use helpers::translate_sql;
use pg2sqlite::prelude::{Pg2SqliteOptions, TranslationOptions, UuidRepresentation};

fn uuid_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}

/// Applies every translated statement to a fresh in-memory SQLite.
fn apply(pg: &str, options: &Pg2SqliteOptions) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for s in helpers::translate_statements(pg, options).expect("translate") {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("SQLite rejected output: {e}\n{s}"));
    }
    conn
}

/// Reads `(hex(id), flagged)` pairs from the audit table in insertion order.
fn audit_rows(conn: &rusqlite::Connection) -> Vec<(String, i64)> {
    let mut stmt = conn
        .prepare("SELECT hex(id), flagged FROM audit ORDER BY rowid")
        .expect("audit table must exist");
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
        .expect("query audit");
    rows.collect::<Result<Vec<_>, _>>().expect("read audit rows")
}

#[test]
fn plpgsql_cte_injection_inside_case() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val INTEGER);
        CREATE TABLE audit (id BLOB PRIMARY KEY, flagged INTEGER);
        CREATE OR REPLACE FUNCTION case_fn() RETURNS TRIGGER AS $$
        DECLARE
            result INTEGER;
        BEGIN
            WITH counts AS (SELECT COUNT(*) AS cnt FROM t)
            SELECT CASE WHEN (SELECT cnt FROM counts) > 0 THEN 1 ELSE 0 END
            INTO result;
            INSERT INTO audit (id, flagged) VALUES (NEW.id, result);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER case_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION case_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    assert!(sql.contains("counts"), "CTE 'counts' should appear in translated output: {sql}");
    let conn = apply(pg, &options);
    // The CASE reads COUNT(*) at BEFORE INSERT time: 0 rows for the first
    // insert, 1 row for the second.
    conn.execute_batch("INSERT INTO t (id, val) VALUES (X'01', 10);").expect("first insert");
    conn.execute_batch("INSERT INTO t (id, val) VALUES (X'02', 20);").expect("second insert");
    assert_eq!(
        audit_rows(&conn),
        vec![("01".to_string(), 0), ("02".to_string(), 1)],
        "the bound CASE result must see the pre-insert row count"
    );
}

#[test]
fn plpgsql_cte_injection_inside_inlist() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val INTEGER);
        CREATE TABLE audit (id BLOB PRIMARY KEY, flagged INTEGER);
        CREATE OR REPLACE FUNCTION inlist_fn() RETURNS TRIGGER AS $$
        DECLARE
            found BOOLEAN;
        BEGIN
            WITH ids AS (SELECT id FROM t)
            SELECT NEW.id IN (SELECT id FROM ids)
            INTO found;
            INSERT INTO audit (id, flagged) VALUES (NEW.id, found);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER inlist_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION inlist_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    assert!(sql.contains("ids"), "CTE 'ids' should appear in translated output: {sql}");
    let conn = apply(pg, &options);
    // A fresh primary key is never already present, so both lookups answer 0,
    // which still proves the IN subquery prepared and ran against the CTE.
    conn.execute_batch("INSERT INTO t (id, val) VALUES (X'01', 10);").expect("first insert");
    conn.execute_batch("INSERT INTO t (id, val) VALUES (X'02', 20);").expect("second insert");
    assert_eq!(audit_rows(&conn), vec![("01".to_string(), 0), ("02".to_string(), 0)]);
}

#[test]
fn plpgsql_cte_injection_inside_between() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val INTEGER);
        CREATE TABLE audit (id BLOB PRIMARY KEY, flagged INTEGER);
        CREATE OR REPLACE FUNCTION between_fn() RETURNS TRIGGER AS $$
        DECLARE
            found BOOLEAN;
        BEGIN
            WITH bounds AS (SELECT 1 AS lo, 10 AS hi)
            SELECT NEW.val BETWEEN (SELECT lo FROM bounds) AND (SELECT hi FROM bounds)
            INTO found;
            INSERT INTO audit (id, flagged) VALUES (NEW.id, found);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER between_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION between_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    assert!(sql.contains("bounds"), "CTE 'bounds' should appear in translated output: {sql}");
    let conn = apply(pg, &options);
    conn.execute_batch("INSERT INTO t (id, val) VALUES (X'01', 5);").expect("in-range insert");
    conn.execute_batch("INSERT INTO t (id, val) VALUES (X'02', 50);").expect("out-of-range insert");
    assert_eq!(
        audit_rows(&conn),
        vec![("01".to_string(), 1), ("02".to_string(), 0)],
        "BETWEEN over the CTE bounds must discriminate in-range from out-of-range"
    );
}

/// The sibling door found while fixing R125: `WITH ... SELECT ... INTO var
/// FROM ...` used to mangle, because the transform keyed on the first
/// `SELECT` substring, which is the CTE body's, and split the statement
/// around it.
#[test]
fn a_with_prefixed_select_into_with_from_binds_and_runs() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val INTEGER);
        CREATE TABLE audit (id BLOB PRIMARY KEY, flagged INTEGER);
        CREATE OR REPLACE FUNCTION wf_fn() RETURNS TRIGGER AS $$
        DECLARE
            result INTEGER;
        BEGIN
            WITH counts AS (SELECT COUNT(*) AS cnt FROM t)
            SELECT cnt INTO result FROM counts;
            INSERT INTO audit (id, flagged) VALUES (NEW.id, result);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER wf_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION wf_fn();
        "#;
    let conn = apply(pg, &options);
    conn.execute_batch("INSERT INTO t (id, val) VALUES (X'01', 10);").expect("first insert");
    conn.execute_batch("INSERT INTO t (id, val) VALUES (X'02', 20);").expect("second insert");
    assert_eq!(
        audit_rows(&conn),
        vec![("01".to_string(), 0), ("02".to_string(), 1)],
        "the FROM-carrying shape must bind the pre-insert row count"
    );
}

/// The minimal door: no WITH, no FROM. `SELECT <expr> INTO var` is the
/// plpgsql spelling of `var := <expr>` and used to survive verbatim.
#[test]
fn a_plain_expression_select_into_binds_and_runs() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val INTEGER);
        CREATE TABLE audit (id BLOB PRIMARY KEY, flagged INTEGER);
        CREATE OR REPLACE FUNCTION plain_fn() RETURNS TRIGGER AS $$
        DECLARE
            result INTEGER;
        BEGIN
            SELECT 1 + 2 INTO result;
            INSERT INTO audit (id, flagged) VALUES (NEW.id, result);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER plain_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION plain_fn();
        "#;
    let conn = apply(pg, &options);
    conn.execute_batch("INSERT INTO t (id, val) VALUES (X'01', 10);").expect("insert");
    assert_eq!(audit_rows(&conn), vec![("01".to_string(), 3)]);
}
