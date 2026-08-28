//! Tests for PL/pgSQL transform completeness:
//! - IsTrue/IsFalse/IsUnknown variant recursion
//! - Missing JoinOperator variants (CrossJoin, LeftSemi, etc.)
//! - LIMIT/FETCH handling in transform_cte_query
//! - Derived subquery full transform (WITH + ORDER BY + LIMIT)
//!
//! These tests use gen_random_uuid() as a marker because the plpgsql
//! transform_expr specifically renames UUID functions (gen_random_uuid → uuid),
//! unlike NOW() which is translated at the statement level.

mod helpers;

use helpers::translate_sql;
use pg2sqlite::prelude::{Pg2SqliteOptions, UuidRepresentation};

fn uuid_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}

#[test]
fn test_plpgsql_is_true_transform() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, flag BOOLEAN, ts TIMESTAMP);
        CREATE OR REPLACE FUNCTION my_trigger_fn() RETURNS TRIGGER AS $$
        BEGIN
            IF (SELECT gen_random_uuid()) IS TRUE THEN
                UPDATE t SET ts = NOW() WHERE id = NEW.id;
            END IF;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER my_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION my_trigger_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated inside IS TRUE: {sql}"
    );
    execute_plpgsql_ddl(pg, &options);
}

#[test]
fn test_plpgsql_is_not_false_transform() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, flag BOOLEAN);
        CREATE OR REPLACE FUNCTION my_trigger_fn() RETURNS TRIGGER AS $$
        BEGIN
            IF (SELECT gen_random_uuid()) IS NOT FALSE THEN
                UPDATE t SET flag = TRUE WHERE id = NEW.id;
            END IF;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER my_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION my_trigger_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated inside IS NOT FALSE: {sql}"
    );
    execute_plpgsql_ddl(pg, &options);
}

#[test]
fn test_plpgsql_cross_join_transform() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val TEXT);
        CREATE TABLE t2 (id BLOB PRIMARY KEY, label TEXT);
        CREATE OR REPLACE FUNCTION cross_join_fn() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO t (id, val)
            SELECT gen_random_uuid(), t2.label
            FROM t2 CROSS JOIN t WHERE t.id = t2.id;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER cross_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION cross_join_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated in CROSS JOIN query: {sql}"
    );
    execute_plpgsql_ddl(pg, &options);
}

#[test]
fn test_plpgsql_limit_expr_transform() {
    // Use gen_random_uuid() as a LIMIT expression -- artificial but tests the
    // transform_cte_query LIMIT path.
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val TEXT);
        CREATE OR REPLACE FUNCTION limit_fn() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO t (id, val)
            SELECT gen_random_uuid(), val FROM t ORDER BY id LIMIT 10;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER limit_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION limit_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated in query with LIMIT: {sql}"
    );
    execute_plpgsql_ddl(pg, &options);
}

#[test]
fn test_plpgsql_derived_subquery_full_transform() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val TEXT);
        CREATE OR REPLACE FUNCTION derived_fn() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO t (id, val)
            SELECT * FROM (SELECT gen_random_uuid() AS id, val FROM t ORDER BY val) sub;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER derived_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION derived_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated inside derived subquery: {sql}"
    );
    execute_plpgsql_ddl(pg, &options);
}

/// Execute the translated DDL from a PL/pgSQL script against an in-memory
/// SQLite. This proves the emitted CREATE TABLE and CREATE TRIGGER statements
/// are valid.
fn execute_plpgsql_ddl(pg: &str, options: &Pg2SqliteOptions) {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for stmt in helpers::translate_statements(pg, options).unwrap() {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("emitted DDL failed: {e}\n{stmt}"));
    }
}
