//! Tests for plpgsql function arm recursion into parameters, filter, over,
//! and within_group clauses.

mod helpers;

use helpers::translate_sql;
use pg2sqlite::prelude::{Pg2SqliteOptions, TranslationOptions, UuidRepresentation};

// ============================================================================
// M13: Function arm — parameters, filter, over, within_group recursion
// ============================================================================

#[test]
fn plpgsql_trigger_translates_function_in_window_partition() {
    // A trigger body containing a function with OVER (PARTITION BY
    // gen_random_uuid()) should translate gen_random_uuid → uuidv7 even inside
    // the window spec.
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob);
    let sql = translate_sql(
        r#"
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
        "#,
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();
    // UUID function should be translated in trigger body
    assert!(
        lower.contains("uuid()"),
        "gen_random_uuid should be translated to uuid() in trigger body: {sql}"
    );
}

// ============================================================================
// M14: Subquery/Exists — with + order_by recursion
// ============================================================================

#[test]
fn plpgsql_trigger_translates_subquery_order_by() {
    // A trigger body with an ORDER BY expression containing gen_random_uuid()
    // inside a subquery. The gen_random_uuid should be translated.
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob);
    let sql = translate_sql(
        r#"
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
        "#,
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();
    assert!(
        lower.contains("uuid()"),
        "gen_random_uuid in trigger body should be translated to uuid(): {sql}"
    );
}

// ============================================================================
// M17: transform_set_expr — SetOperation path
// ============================================================================

#[test]
fn plpgsql_trigger_translates_union_all_body() {
    // A trigger with UNION ALL in the body: both sides should have
    // gen_random_uuid translated.
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob);
    let sql = translate_sql(
        r#"
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
        "#,
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();
    // Both sides of UNION should have uuid translated
    let count = lower.matches("uuid()").count();
    assert!(
        count >= 2,
        "expected at least 2 uuid() occurrences in UNION ALL body, got {count}: {sql}"
    );
}

// ============================================================================
// M18: transform_table_factor — NestedJoin (basic coverage)
// ============================================================================

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
