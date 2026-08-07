//! Tests for PL/pgSQL inject_with_into_in_subqueries missing arms (GROUP G).
//!
//! G1: Case, InList, Between, Cast, Function, Tuple arms missing from
//! catch-all. When a CTE-referencing subquery is nested inside one of these
//! expressions, the WITH clause must be injected into the subquery.

mod helpers;

use helpers::translate_sql;
use pg2sqlite::prelude::{Pg2SqliteOptions, TranslationOptions, UuidRepresentation};

fn uuid_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}

#[test]
fn plpgsql_cte_injection_inside_case() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val INTEGER);
        CREATE OR REPLACE FUNCTION case_fn() RETURNS TRIGGER AS $$
        DECLARE
            result INTEGER;
        BEGIN
            WITH counts AS (SELECT COUNT(*) AS cnt FROM t)
            SELECT CASE WHEN (SELECT cnt FROM counts) > 0 THEN 1 ELSE 0 END
            INTO result;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER case_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION case_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    assert!(sql.contains("counts"), "CTE 'counts' should appear in translated output: {sql}");
    // Pins R125: SELECT ... INTO var survives in trigger body and SQLite refuses
    // it. Goes red when the defect is fixed, which is the point.
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    let mut pin_triggered = false;
    for s in helpers::translate_statements(pg, &options).expect("translate") {
        let result = conn.execute_batch(&format!("{s};"));
        if let Err(err) = &result {
            assert!(
                err.to_string().contains("near \"INTO\": syntax error"),
                "expected INTO syntax error: {err}"
            );
            pin_triggered = true;
        }
    }
    assert!(pin_triggered, "R125 pin: expected trigger with SELECT INTO to be refused by SQLite");
}

#[test]
fn plpgsql_cte_injection_inside_inlist() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val INTEGER);
        CREATE OR REPLACE FUNCTION inlist_fn() RETURNS TRIGGER AS $$
        DECLARE
            found BOOLEAN;
        BEGIN
            WITH ids AS (SELECT id FROM t)
            SELECT NEW.id IN (SELECT id FROM ids)
            INTO found;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER inlist_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION inlist_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    assert!(sql.contains("ids"), "CTE 'ids' should appear in translated output: {sql}");
    // Pins R125: SELECT ... INTO var survives in trigger body and SQLite refuses
    // it. Goes red when the defect is fixed, which is the point.
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    let mut pin_triggered = false;
    for s in helpers::translate_statements(pg, &options).expect("translate") {
        let result = conn.execute_batch(&format!("{s};"));
        if let Err(err) = &result {
            assert!(
                err.to_string().contains("near \"INTO\": syntax error"),
                "expected INTO syntax error: {err}"
            );
            pin_triggered = true;
        }
    }
    assert!(pin_triggered, "R125 pin: expected trigger with SELECT INTO to be refused by SQLite");
}

#[test]
fn plpgsql_cte_injection_inside_between() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val INTEGER);
        CREATE OR REPLACE FUNCTION between_fn() RETURNS TRIGGER AS $$
        DECLARE
            found BOOLEAN;
        BEGIN
            WITH bounds AS (SELECT 1 AS lo, 10 AS hi)
            SELECT NEW.val BETWEEN (SELECT lo FROM bounds) AND (SELECT hi FROM bounds)
            INTO found;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER between_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION between_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    assert!(sql.contains("bounds"), "CTE 'bounds' should appear in translated output: {sql}");
    // Pins R125: SELECT ... INTO var survives in trigger body and SQLite refuses
    // it. Goes red when the defect is fixed, which is the point.
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    let mut pin_triggered = false;
    for s in helpers::translate_statements(pg, &options).expect("translate") {
        let result = conn.execute_batch(&format!("{s};"));
        if let Err(err) = &result {
            assert!(
                err.to_string().contains("near \"INTO\": syntax error"),
                "expected INTO syntax error: {err}"
            );
            pin_triggered = true;
        }
    }
    assert!(pin_triggered, "R125 pin: expected trigger with SELECT INTO to be refused by SQLite");
}
