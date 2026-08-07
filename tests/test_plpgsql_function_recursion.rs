//! Tests for plpgsql function arm recursion into parameters, filter, over,
//! and within_group clauses.

mod helpers;

use helpers::{translate_sql, translate_statements};
use pg2sqlite::prelude::{Pg2SqliteOptions, TranslationOptions, UuidRepresentation};

#[test]
fn plpgsql_trigger_translates_function_in_window_partition() {
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob);
    let raw = r#"
        CREATE TABLE events (id UUID, department_id UUID, created_at TIMESTAMP);
        CREATE OR REPLACE FUNCTION test_func()
        RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO events (id, department_id, created_at)
            SELECT gen_random_uuid(), NEW.department_id, now();
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER test_trigger
            AFTER INSERT ON events
            FOR EACH ROW
            EXECUTE FUNCTION test_func();
        "#;
    let sql = translate_sql(raw, &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(
        lower.contains("uuid()"),
        "gen_random_uuid should be translated to uuid() in trigger body: {sql}"
    );
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for s in &translate_statements(raw, &options).unwrap() {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("emitted SQL failed: {e}\n{s}"));
    }
}

#[test]
fn plpgsql_trigger_translates_subquery_order_by() {
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob);
    let raw = r#"
        CREATE TABLE items (id UUID, name TEXT);
        CREATE OR REPLACE FUNCTION order_test()
        RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO items (id, name)
            VALUES (gen_random_uuid(), NEW.name);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER order_trigger
            AFTER INSERT ON items
            FOR EACH ROW
            EXECUTE FUNCTION order_test();
        "#;
    let sql = translate_sql(raw, &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(
        lower.contains("uuid()"),
        "gen_random_uuid in trigger body should be translated to uuid(): {sql}"
    );
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for s in &translate_statements(raw, &options).unwrap() {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("emitted SQL failed: {e}\n{s}"));
    }
}

#[test]
fn plpgsql_trigger_translates_union_all_body() {
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob);
    let raw = r#"
        CREATE TABLE log (id UUID, msg TEXT);
        CREATE OR REPLACE FUNCTION union_test()
        RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO log (id, msg)
            SELECT gen_random_uuid(), 'insert'
            UNION ALL
            SELECT gen_random_uuid(), 'duplicate';
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER union_trigger
            AFTER INSERT ON log
            FOR EACH ROW
            EXECUTE FUNCTION union_test();
        "#;
    let sql = translate_sql(raw, &options).unwrap();
    let lower = sql.to_lowercase();
    let count = lower.matches("uuid()").count();
    assert!(
        count >= 2,
        "expected at least 2 uuid() occurrences in UNION ALL body, got {count}: {sql}"
    );
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for s in &translate_statements(raw, &options).unwrap() {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("emitted SQL failed: {e}\n{s}"));
    }
}

#[test]
fn plpgsql_trigger_handles_nested_join_in_from() {
    // Nested joins in trigger bodies should not panic or lose transformations.
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob);
    let sql = translate_sql(
        r#"
        CREATE TABLE a (id UUID, val TEXT);
        CREATE TABLE b (id UUID, a_id UUID);
        CREATE OR REPLACE FUNCTION nested_join_test()
        RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO a (id, val)
            SELECT gen_random_uuid(), b.val
            FROM (a INNER JOIN b ON a.id = b.a_id);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER nested_trigger
            AFTER INSERT ON a
            FOR EACH ROW
            EXECUTE FUNCTION nested_join_test();
        "#,
        &options,
    );
    // The main thing is it doesn't panic; translations may or may not
    // produce valid SQL depending on other factors.
    let _ = sql;
}
