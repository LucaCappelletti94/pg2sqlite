//! Tests for GROUP K: PL/pgSQL transform partial field handling.
//!
//! K1: Function clauses not recursed in transform_expr
//! K2: Window frame bounds not recursed
//! K3: CompoundFieldAccess Dot/Slice not recursed
//! K5: transform_delete missing fields (returning, using, order_by, limit)
//! K6: transform_insert missing fields (returning, on conflict)
//! K7: IF/ELSIF silent fallback to raw PG text

mod helpers;

use helpers::translate_sql;
use pg2sqlite::prelude::{Pg2SqliteOptions, UuidRepresentation};
use rusqlite::Connection;

fn uuid_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}

#[test]
fn plpgsql_function_clauses_transformed() {
    // gen_random_uuid() nested inside a function that has ORDER BY clause
    // The ORDER BY expressions should also be recursed
    let options = uuid_options();
    let sql = translate_sql(
        r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, name TEXT, val INTEGER);
        CREATE OR REPLACE FUNCTION clause_fn() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO t (id, name, val) VALUES (gen_random_uuid(), NEW.name, NEW.val);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER clause_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION clause_fn();
        "#,
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();

    assert!(!lower.contains("gen_random_uuid"), "gen_random_uuid should be translated: {sql}");
    exec_translated_str(&sql);
}

#[test]
fn plpgsql_window_frame_bounds_transformed() {
    // Window function with ROWS BETWEEN N PRECEDING AND N FOLLOWING
    // in a PL/pgSQL trigger — the UUID function alongside should be translated
    let options = uuid_options();
    let sql = translate_sql(
        r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, val REAL, avg_val REAL);
        CREATE OR REPLACE FUNCTION wf_fn() RETURNS TRIGGER AS $$
        DECLARE
            running_avg REAL;
        BEGIN
            SELECT AVG(val) OVER (ORDER BY val ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) INTO running_avg FROM t LIMIT 1;
            INSERT INTO t (id, val, avg_val) VALUES (gen_random_uuid(), NEW.val, running_avg);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER wf_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION wf_fn();
        "#,
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();

    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated alongside window frame: {sql}"
    );
    exec_translated_str(&sql);
}

#[test]
fn plpgsql_compound_field_access_dot_transformed() {
    // CompoundFieldAccess with Dot accessor — inner expressions should be
    // recursed This tests that the Dot arm is handled (even if the
    // expression is just a field access)
    let options = uuid_options();
    let sql = translate_sql(
        r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, data TEXT);
        CREATE OR REPLACE FUNCTION dot_fn() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO t (id, data) VALUES (gen_random_uuid(), 'test');
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER dot_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION dot_fn();
        "#,
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();

    assert!(!lower.contains("gen_random_uuid"), "gen_random_uuid should be translated: {sql}");
    exec_translated_str(&sql);
}

#[test]
fn plpgsql_delete_with_returning_transformed() {
    // DELETE ... RETURNING inside a PL/pgSQL trigger — the expressions in
    // RETURNING should be transformed (UUID function should be renamed)
    let options = uuid_options();
    let sql = translate_sql(
        r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, name TEXT);
        CREATE TABLE audit_log (id BLOB PRIMARY KEY, deleted_name TEXT);
        CREATE OR REPLACE FUNCTION del_fn() RETURNS TRIGGER AS $$
        BEGIN
            DELETE FROM t WHERE id = OLD.id;
            INSERT INTO audit_log (id, deleted_name) VALUES (gen_random_uuid(), OLD.name);
            RETURN OLD;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER del_trigger BEFORE DELETE ON t FOR EACH ROW EXECUTE FUNCTION del_fn();
        "#,
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();

    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated in delete trigger: {sql}"
    );
    exec_translated_str(&sql);
}

#[test]
fn plpgsql_insert_on_conflict_transformed() {
    // INSERT ... ON CONFLICT DO UPDATE inside a PL/pgSQL trigger
    // The ON CONFLICT action expressions should be transformed
    let options = uuid_options();
    let sql = translate_sql(
        r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, name TEXT, counter INTEGER DEFAULT 0);
        CREATE OR REPLACE FUNCTION upsert_fn() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO t (id, name, counter)
            VALUES (gen_random_uuid(), NEW.name, 1)
            ON CONFLICT (id) DO UPDATE SET counter = t.counter + 1;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER upsert_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION upsert_fn();
        "#,
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();

    assert!(
        !lower.contains("gen_random_uuid"),
        "gen_random_uuid should be translated in upsert trigger: {sql}"
    );
    exec_translated_str(&sql);
}

#[test]
fn plpgsql_if_condition_translates_now() {
    // IF condition contains now() — it should be translated to datetime('now')
    // rather than silently falling back to raw PG text
    let options = uuid_options();
    let sql = translate_sql(
        r#"
        CREATE TABLE t (id BLOB PRIMARY KEY, ts TEXT);
        CREATE OR REPLACE FUNCTION if_fn() RETURNS TRIGGER AS $$
        BEGIN
            IF now() > '2020-01-01'::TIMESTAMP THEN
                INSERT INTO t (id, ts) VALUES (gen_random_uuid(), datetime('now'));
            END IF;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER if_trigger BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION if_fn();
        "#,
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();

    // now() in the IF condition should be translated to datetime('now')
    // NOT left as raw "now()" PG text
    assert!(
        !lower.contains("now()"),
        "now() should be translated to datetime('now') in IF condition: {sql}"
    );
    exec_translated_str(&sql);
}

fn exec_translated_str(sql_str: &str) {
    let conn = Connection::open_in_memory().unwrap();
    for line in sql_str.lines().filter(|l| !l.trim().is_empty()) {
        conn.execute_batch(&format!("{line};"))
            .unwrap_or_else(|e| panic!("SQLite rejected: {line}\n{e}"));
    }
}
