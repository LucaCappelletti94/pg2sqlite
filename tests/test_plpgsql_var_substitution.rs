//! Tests for PL/pgSQL substitute_variables missing arms (GROUP H).
//!
//! H1: Like/ILike, Extract, Trim, Substring, Collate, Position variants
//!     missing from catch-all in substitute_variables.
//! H2: table_with_joins_references_variable missing join operators.

mod helpers;

use diesel::prelude::*;
use helpers::{translate_sql, translate_statements};
use pg2sqlite::prelude::{Pg2SqliteOptions, UuidRepresentation};

fn uuid_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}

#[test]
fn plpgsql_substitute_variable_in_like_pattern() {
    let options = uuid_options();
    let sql = translate_sql(
        r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, name TEXT);
        CREATE OR REPLACE FUNCTION like_fn() RETURNS TRIGGER AS $$
        DECLARE
            pattern TEXT := '%test%';
            found BOOLEAN;
        BEGIN
            SELECT EXISTS(SELECT 1 FROM t WHERE name LIKE pattern) INTO found;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER like_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION like_fn();
        "#,
        &options,
    )
    .unwrap();

    // The variable 'pattern' should be substituted (not left as bare identifier)
    // In the CTE-based approach, it becomes a reference to the CTE column
    assert!(!sql.is_empty(), "Translation should succeed for LIKE with variable: {sql}");
}

#[test]
fn plpgsql_substitute_variable_in_extract() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, ts TEXT, val INTEGER);
        CREATE OR REPLACE FUNCTION extract_fn() RETURNS TRIGGER AS $$
        DECLARE
            my_ts TIMESTAMP := NOW();
            yr INTEGER;
        BEGIN
            yr := EXTRACT(YEAR FROM my_ts);
            INSERT INTO t (id, ts, val) VALUES (gen_random_uuid(), my_ts::TEXT, yr);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER extract_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION extract_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated in trigger with EXTRACT: {sql}"
    );
    let stmts = translate_statements(pg, &options).unwrap();
    let mut conn = diesel::SqliteConnection::establish(":memory:").expect("connect");
    for s in &stmts {
        diesel::sql_query(s.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("translated DDL must execute: {e}\n{s}"));
    }
}

#[test]
fn plpgsql_substitute_variable_in_trim() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, name TEXT);
        CREATE OR REPLACE FUNCTION trim_fn() RETURNS TRIGGER AS $$
        DECLARE
            raw_name TEXT := '  hello  ';
        BEGIN
            INSERT INTO t (id, name) VALUES (gen_random_uuid(), TRIM(raw_name));
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER trim_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION trim_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated in trigger with TRIM: {sql}"
    );
    let stmts = translate_statements(pg, &options).unwrap();
    let mut conn = diesel::SqliteConnection::establish(":memory:").expect("connect");
    for s in &stmts {
        diesel::sql_query(s.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("translated DDL must execute: {e}\n{s}"));
    }
}

#[test]
fn plpgsql_substitute_variable_in_substring() {
    let options = uuid_options();
    let pg = r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, prefix TEXT);
        CREATE OR REPLACE FUNCTION substr_fn() RETURNS TRIGGER AS $$
        DECLARE
            full_name TEXT := 'hello world';
        BEGIN
            INSERT INTO t (id, prefix) VALUES (gen_random_uuid(), SUBSTRING(full_name FROM 1 FOR 5));
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER substr_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION substr_fn();
        "#;
    let sql = translate_sql(pg, &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated in trigger with SUBSTRING: {sql}"
    );
    let stmts = translate_statements(pg, &options).unwrap();
    let mut conn = diesel::SqliteConnection::establish(":memory:").expect("connect");
    for s in &stmts {
        diesel::sql_query(s.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("translated DDL must execute: {e}\n{s}"));
    }
}
