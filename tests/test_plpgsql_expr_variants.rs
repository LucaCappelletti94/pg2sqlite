//! Tests for PL/pgSQL transform_expr missing variant recursion (GROUP F).
//!
//! F1: Collate, Position, Ceil/Floor arms missing from catch-all
//! F2: AtTimeZone only recurses timestamp, not time_zone

mod helpers;

use helpers::translate_sql;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};
use rusqlite::Connection as SqliteConn;

fn uuid_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}

// (sqlparser limitation). Note: the AT TIME ZONE zone operand is always a
// literal, so the F2 recursion into that one field is a defensive change with
// nothing observable to assert. The operator itself is observable, and
// `plpgsql_at_time_zone_is_transformed` checks it.

#[test]
fn plpgsql_combined_expr_types_transformed() {
    // Test multiple expression types together in one trigger
    let options = uuid_options();
    let sql = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val REAL, name TEXT);
        CREATE OR REPLACE FUNCTION combined_fn() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO t (id, val, name)
            VALUES (gen_random_uuid(), CEIL(3.14) + FLOOR(2.7), 'test');
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER combined_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION combined_fn();
        "#;
    let sql_str = translate_sql(sql, &options).unwrap();
    let lower = sql_str.to_lowercase();

    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated alongside CEIL+FLOOR: {sql_str}"
    );
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let conn = SqliteConn::open_in_memory().unwrap();
    conn.execute_batch(&stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

#[test]
fn plpgsql_position_expr_transformed() {
    let options = uuid_options();
    let sql = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val INTEGER);
        CREATE OR REPLACE FUNCTION pos_fn() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO t (id, val) VALUES (gen_random_uuid(), POSITION('x' IN 'xyz'));
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER pos_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION pos_fn();
        "#;
    let sql_str = translate_sql(sql, &options).unwrap();
    let lower = sql_str.to_lowercase();

    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated alongside POSITION: {sql_str}"
    );
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let conn = SqliteConn::open_in_memory().unwrap();
    conn.execute_batch(&stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

#[test]
fn plpgsql_ceil_floor_expr_transformed() {
    let options = uuid_options();
    let sql = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val REAL);
        CREATE OR REPLACE FUNCTION ceil_fn() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO t (id, val) VALUES (gen_random_uuid(), CEIL(3.14));
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER ceil_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION ceil_fn();
        "#;
    let sql_str = translate_sql(sql, &options).unwrap();
    let lower = sql_str.to_lowercase();

    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated alongside CEIL: {sql_str}"
    );
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let conn = SqliteConn::open_in_memory().unwrap();
    conn.execute_batch(&stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}

/// Renamed from `plpgsql_at_time_zone_time_zone_field_transformed`, which
/// asserted only that `gen_random_uuid` was translated. The zone operand is
/// always a literal, so recursing into it changes nothing observable, but the
/// operator itself is observable and is what this now checks.
#[test]
fn plpgsql_at_time_zone_is_transformed() {
    let options = uuid_options();
    let sql = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, ts TEXT);
        CREATE OR REPLACE FUNCTION tz_fn() RETURNS TRIGGER AS $$
        DECLARE
            my_ts TIMESTAMP;
        BEGIN
            my_ts := NOW() AT TIME ZONE 'UTC';
            INSERT INTO t (id, ts) VALUES (gen_random_uuid(), my_ts::TEXT);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER tz_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION tz_fn();
        "#;
    let sql_str = translate_sql(sql, &options).unwrap();
    let lower = sql_str.to_lowercase();

    assert!(
        !lower.contains("at time zone"),
        "AT TIME ZONE has no SQLite equivalent and must not survive: {sql_str}"
    );
    assert!(
        lower.contains("datetime(datetime('now'))"),
        "NOW() AT TIME ZONE 'UTC' should become datetime(datetime('now')): {sql_str}"
    );
    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated in trigger with AT TIME ZONE: {sql_str}"
    );
    // Execute the emitted DDL to prove real SQLite accepts the translated
    // schema.
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let conn = SqliteConn::open_in_memory().unwrap();
    conn.execute_batch(&stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|e| panic!("translated DDL must execute: {e}"));
}
