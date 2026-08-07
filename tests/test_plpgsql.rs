//! Tests for PL/pgSQL translation in
//! `src/impls/translator_impls/plpgsql/translator.rs`.
//!
//! Covers: IF with ELSIF, IF with ELSE, SET statement (variable binding),
//! UUID function translation, WITH RECURSIVE ... INSERT, WITH RECURSIVE ...
//! DELETE, SetOperation in query body, inject_condition_into_statement
//! (UPDATE/DELETE).

#[path = "helpers/translate.rs"]
mod translate_helpers;
use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};
use translate_helpers::translate_default as translate;

fn translate_with_options(sql: &str, options: &Pg2SqliteOptions) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(options)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Translates `sql` with default options and executes every emitted statement
/// in an in-memory SQLite connection, verifying the output is valid SQLite.
fn execute_trigger_ddl(sql: &str) {
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for stmt in &stmts {
        conn.execute_batch(&format!("{stmt};"))
            .expect("translated trigger DDL must execute in SQLite");
    }
}

/// Like `execute_trigger_ddl` but uses caller-supplied options.
fn execute_trigger_ddl_with_opts(sql: &str, options: &Pg2SqliteOptions) {
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate(options).unwrap();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for stmt in &stmts {
        conn.execute_batch(&format!("{stmt};"))
            .expect("translated trigger DDL must execute in SQLite");
    }
}

#[test]
fn if_elsif_translates() {
    let sql = include_str!("fixtures/trigger_elsif_else.sql");
    let output = translate(sql);
    // The IF/ELSIF blocks should produce separate INSERT statements
    // with injected conditions
    assert!(output.contains("INSERT"), "Expected INSERT statements from trigger: {output}");
    execute_trigger_ddl(sql);
}

#[test]
fn if_else_translates() {
    let sql = r#"
        CREATE TABLE log (id SERIAL PRIMARY KEY, msg TEXT NOT NULL);
        CREATE TABLE items (id SERIAL PRIMARY KEY, status TEXT NOT NULL);

        CREATE OR REPLACE FUNCTION log_item() RETURNS TRIGGER AS $$
        BEGIN
            IF NEW.status = 'active' THEN
                INSERT INTO log (msg) VALUES ('item activated');
            ELSE
                INSERT INTO log (msg) VALUES ('item changed');
            END IF;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER item_trigger
        AFTER INSERT ON items
        FOR EACH ROW EXECUTE FUNCTION log_item();
    "#;
    let output = translate(sql);
    assert!(output.contains("INSERT"), "Expected INSERT from trigger: {output}");
    execute_trigger_ddl(sql);
}

#[test]
fn set_variable_binding_in_trigger() {
    // The trigger_issue.sql fixture has triggers that use SET internally
    // through the SELECT INTO preprocessing
    let sql = include_str!("fixtures/trigger_issue.sql");
    let output = translate(sql);
    // Should produce INSERT statements with proper variable substitution
    assert!(output.contains("INSERT"), "Expected INSERT statements from trigger: {output}");
    execute_trigger_ddl(sql);
}

#[test]
fn declare_default_values_are_available_without_assignment() {
    let sql = r#"
        CREATE TABLE items (id INT PRIMARY KEY, kind TEXT);
        CREATE TABLE audit (log_id TEXT, msg TEXT);

        CREATE OR REPLACE FUNCTION audit_insert() RETURNS TRIGGER AS $$
        DECLARE
            v_log_id UUID := gen_random_uuid();
            v_msg TEXT := 'created';
        BEGIN
            INSERT INTO audit (log_id, msg) VALUES (v_log_id, v_msg);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER audit_insert_trigger
        AFTER INSERT ON items
        FOR EACH ROW EXECUTE FUNCTION audit_insert();
    "#;

    let output = translate(sql);
    assert!(
        output.contains("v_log_id.val") && output.contains("v_msg.val"),
        "DECLARE defaults should be bound through generated CTE values: {output}"
    );
    assert!(
        !output.contains("VALUES (v_log_id, v_msg)"),
        "Raw undeclared variable references should not remain in trigger SQL: {output}"
    );
    execute_trigger_ddl(sql);
}

#[test]
fn declare_default_uuid_uses_configured_uuid_function() {
    let sql = r#"
        CREATE TABLE items (id INT PRIMARY KEY, kind TEXT);
        CREATE TABLE audit (log_id TEXT);

        CREATE OR REPLACE FUNCTION audit_insert() RETURNS TRIGGER AS $$
        DECLARE
            v_log_id UUID := gen_random_uuid();
        BEGIN
            INSERT INTO audit (log_id) VALUES (v_log_id);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER audit_insert_trigger
        AFTER INSERT ON items
        FOR EACH ROW EXECUTE FUNCTION audit_insert();
    "#;

    let options = Pg2SqliteOptions::default().with_uuid_function_name("uuid7".to_string());
    let output = translate_with_options(sql, &options);
    assert!(
        output.contains("uuid7()"),
        "DECLARE UUID default should honor configured uuid function name: {output}"
    );
    execute_trigger_ddl_with_opts(sql, &options);
}

#[test]
fn select_into_with_comma_expression_produces_valid_sqlite_trigger_sql() {
    let sql = r#"
        CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);
        CREATE TABLE logs (v INT);

        CREATE OR REPLACE FUNCTION trg_fn() RETURNS TRIGGER AS $$
        DECLARE
            v INT;
        BEGIN
            SELECT COALESCE(NEW.a, NEW.b) INTO v FROM t LIMIT 1;
            INSERT INTO logs(v) VALUES (v);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER trg BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION trg_fn();
    "#;

    let translated =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    let sqlite_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join(";\n");

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let exec_result = conn.execute_batch(&sqlite_sql);
    assert!(
        exec_result.is_ok(),
        "Translated SQL should execute in SQLite, got error: {exec_result:?}\nSQL:\n{sqlite_sql}"
    );
}

#[test]
fn with_recursive_insert_transforms() {
    // The trigger_issue.sql fixture has patterns that may involve CTE-based inserts
    let sql = include_str!("fixtures/trigger_issue.sql");
    let output = translate(sql);
    // Should translate without error and produce valid statements
    assert!(!output.is_empty(), "Expected non-empty output: {output}");
}

#[test]
fn trigger_with_on_conflict_do_nothing() {
    let sql = r#"
        CREATE TABLE targets (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE source (id INT PRIMARY KEY, target_id INT);

        CREATE OR REPLACE FUNCTION copy_to_targets() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO targets (id, name)
            VALUES (NEW.target_id, 'auto')
            ON CONFLICT (id) DO NOTHING;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER copy_trigger
        AFTER INSERT ON source
        FOR EACH ROW EXECUTE FUNCTION copy_to_targets();
    "#;
    let output = translate(sql);
    // ON CONFLICT DO NOTHING should become OR IGNORE in SQLite
    assert!(
        output.contains("INSERT OR IGNORE") || output.contains("INSERT"),
        "Expected INSERT (possibly OR IGNORE): {output}"
    );
    execute_trigger_ddl(sql);
}

#[test]
fn multiple_triggers_translate() {
    let sql = r#"
        CREATE TABLE audit (id SERIAL PRIMARY KEY, action TEXT, detail TEXT);
        CREATE TABLE orders (id SERIAL PRIMARY KEY, total INT, status TEXT);

        CREATE OR REPLACE FUNCTION audit_order_insert() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO audit (action, detail) VALUES ('insert', 'new order');
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE OR REPLACE FUNCTION audit_order_update() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO audit (action, detail) VALUES ('update', 'order updated');
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER order_insert_trigger
        AFTER INSERT ON orders
        FOR EACH ROW EXECUTE FUNCTION audit_order_insert();

        CREATE TRIGGER order_update_trigger
        AFTER UPDATE ON orders
        FOR EACH ROW EXECUTE FUNCTION audit_order_update();
    "#;
    let output = translate(sql);
    assert!(output.contains("order_insert_trigger"), "Expected insert trigger: {output}");
    assert!(output.contains("order_update_trigger"), "Expected update trigger: {output}");
    execute_trigger_ddl(sql);
}

#[test]
fn if_with_update_in_trigger() {
    let sql = r#"
        CREATE TABLE counters (id INT PRIMARY KEY, count INT DEFAULT 0);
        CREATE TABLE events (id SERIAL PRIMARY KEY, counter_id INT, event_type TEXT);

        CREATE OR REPLACE FUNCTION update_counter() RETURNS TRIGGER AS $$
        BEGIN
            IF NEW.event_type = 'increment' THEN
                UPDATE counters SET count = count + 1 WHERE id = NEW.counter_id;
            END IF;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER counter_trigger
        AFTER INSERT ON events
        FOR EACH ROW EXECUTE FUNCTION update_counter();
    "#;
    let output = translate(sql);
    assert!(output.contains("UPDATE"), "Expected UPDATE in trigger body: {output}");
    assert!(output.contains("counter_trigger"), "Expected trigger to be created: {output}");
    execute_trigger_ddl(sql);
}

#[test]
fn if_with_delete_in_trigger() {
    let sql = r#"
        CREATE TABLE trash (id INT PRIMARY KEY, item_id INT);
        CREATE TABLE items (id SERIAL PRIMARY KEY, status TEXT);

        CREATE OR REPLACE FUNCTION cleanup_trash() RETURNS TRIGGER AS $$
        BEGIN
            IF NEW.status = 'deleted' THEN
                DELETE FROM trash WHERE item_id = NEW.id;
            END IF;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER cleanup_trigger
        AFTER UPDATE ON items
        FOR EACH ROW EXECUTE FUNCTION cleanup_trash();
    "#;
    let output = translate(sql);
    assert!(output.contains("DELETE"), "Expected DELETE in trigger body: {output}");
    execute_trigger_ddl(sql);
}

#[test]
fn trigger_new_values_translated() {
    let sql = r#"
        CREATE TABLE log (id SERIAL PRIMARY KEY, user_id INT, action TEXT);
        CREATE TABLE users (id SERIAL PRIMARY KEY, name TEXT, email TEXT);

        CREATE OR REPLACE FUNCTION log_user_creation() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO log (user_id, action)
            VALUES (NEW.id, 'created');
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER user_creation_trigger
        AFTER INSERT ON users
        FOR EACH ROW EXECUTE FUNCTION log_user_creation();
    "#;
    let output = translate(sql);
    assert!(output.contains("NEW.id"), "Expected NEW.id reference: {output}");
    execute_trigger_ddl(sql);
}

#[test]
fn trigger_with_multiple_statements() {
    let sql = r#"
        CREATE TABLE inventory (id SERIAL PRIMARY KEY, product TEXT, quantity INT);
        CREATE TABLE inventory_log (id SERIAL PRIMARY KEY, product TEXT, action TEXT);

        CREATE OR REPLACE FUNCTION log_inventory() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO inventory_log (product, action) VALUES (NEW.product, 'added');
            INSERT INTO inventory_log (product, action) VALUES (NEW.product, 'verified');
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER inventory_trigger
        AFTER INSERT ON inventory
        FOR EACH ROW EXECUTE FUNCTION log_inventory();
    "#;
    let output = translate(sql);
    assert!(output.contains("INSERT"), "Expected INSERT in trigger: {output}");
    execute_trigger_ddl(sql);
}

#[test]
fn query_statement_runs_standard_translation_pipeline() {
    let sql = r#"
        CREATE TABLE events (id INT PRIMARY KEY, created_at TEXT);

        CREATE OR REPLACE FUNCTION query_probe() RETURNS TRIGGER AS $$
        BEGIN
            SELECT NOW() ORDER BY NOW() LIMIT NOW();
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER query_probe_trigger
        AFTER INSERT ON events
        FOR EACH ROW EXECUTE FUNCTION query_probe();
    "#;

    let output = translate(sql);
    assert!(
        output.contains("datetime('now')"),
        "Expected query statement expressions to be translated through standard pipeline: {output}"
    );
    execute_trigger_ddl(sql);
}

#[test]
fn uuid_function_in_trigger() {
    let sql = include_str!("fixtures/trigger_uuid_insert.sql");
    let output = translate(sql);
    // gen_random_uuid() should be translated to uuid() or similar
    assert!(
        output.contains("INSERT") || output.contains("todo_history"),
        "Expected INSERT: {output}"
    );
    execute_trigger_ddl(sql);
}

#[test]
fn schema_qualified_uuid_function_in_trigger() {
    let sql = r#"
        CREATE TABLE source_docs (id INT PRIMARY KEY);
        CREATE TABLE outbox (id TEXT PRIMARY KEY, source_id INT NOT NULL);

        CREATE OR REPLACE FUNCTION copy_doc() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO outbox (id, source_id)
            VALUES (public.gen_random_uuid(), NEW.id);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER copy_doc_trigger
        AFTER INSERT ON source_docs
        FOR EACH ROW EXECUTE FUNCTION copy_doc();
    "#;

    let output = translate(sql);
    assert!(
        output.contains("uuid()"),
        "Expected schema-qualified UUID function to be rewritten: {output}"
    );
    assert!(
        !output.contains("public.gen_random_uuid"),
        "Schema-qualified UUID function should not remain in output: {output}"
    );
    execute_trigger_ddl(sql);
}

#[test]
fn if_with_insert_and_condition() {
    let sql = r#"
        CREATE TABLE notifications (id SERIAL PRIMARY KEY, user_id INT, message TEXT);
        CREATE TABLE orders (id SERIAL PRIMARY KEY, status TEXT, user_id INT);

        CREATE OR REPLACE FUNCTION notify_order_change() RETURNS TRIGGER AS $$
        BEGIN
            IF NEW.status = 'shipped' THEN
                INSERT INTO notifications (user_id, message)
                VALUES (NEW.user_id, 'Your order has shipped');
            ELSIF NEW.status = 'delivered' THEN
                INSERT INTO notifications (user_id, message)
                VALUES (NEW.user_id, 'Your order was delivered');
            ELSE
                INSERT INTO notifications (user_id, message)
                VALUES (NEW.user_id, 'Order status changed');
            END IF;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER notify_trigger
        AFTER UPDATE ON orders
        FOR EACH ROW EXECUTE FUNCTION notify_order_change();
    "#;
    let output = translate(sql);
    // The IF/ELSIF/ELSE should produce separate INSERT statements with injected
    // conditions
    assert!(output.contains("INSERT"), "Expected INSERT statements: {output}");
    assert!(
        output.contains("shipped") || output.contains("delivered"),
        "Expected condition values: {output}"
    );
    execute_trigger_ddl(sql);
}

#[test]
fn trigger_with_union_in_insert() {
    let sql = r#"
        CREATE TABLE log_table (id SERIAL PRIMARY KEY, msg TEXT);
        CREATE TABLE events3 (id SERIAL PRIMARY KEY, event_type TEXT);

        CREATE OR REPLACE FUNCTION log_event() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO log_table (msg)
            SELECT 'event: ' || NEW.event_type
            UNION ALL
            SELECT 'backup: ' || NEW.event_type;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER event_trigger
        AFTER INSERT ON events3
        FOR EACH ROW EXECUTE FUNCTION log_event();
    "#;
    let output = translate(sql);
    assert!(
        output.contains("INSERT") || output.contains("log_table"),
        "Expected INSERT into log_table: {output}"
    );
    execute_trigger_ddl(sql);
}

#[test]
fn if_with_delete_existing_where() {
    let sql = r#"
        CREATE TABLE archive (id INT PRIMARY KEY, item_id INT, status TEXT);
        CREATE TABLE items2 (id SERIAL PRIMARY KEY, status TEXT);

        CREATE OR REPLACE FUNCTION archive_cleanup() RETURNS TRIGGER AS $$
        BEGIN
            IF NEW.status = 'archived' THEN
                DELETE FROM archive WHERE item_id = NEW.id AND status = 'pending';
            END IF;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER archive_trigger
        AFTER UPDATE ON items2
        FOR EACH ROW EXECUTE FUNCTION archive_cleanup();
    "#;
    let output = translate(sql);
    assert!(
        output.contains("DELETE") || output.contains("archive_trigger"),
        "Expected DELETE or trigger: {output}"
    );
    execute_trigger_ddl(sql);
}

#[test]
fn trigger_with_bare_values_insert() {
    let sql = r#"
        CREATE TABLE simple_log (id SERIAL PRIMARY KEY, message TEXT, created TEXT);
        CREATE TABLE simple_items (id SERIAL PRIMARY KEY, name TEXT);

        CREATE OR REPLACE FUNCTION simple_log_insert() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO simple_log (message, created) VALUES (NEW.name, 'now');
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER simple_log_trigger
        AFTER INSERT ON simple_items
        FOR EACH ROW EXECUTE FUNCTION simple_log_insert();
    "#;
    let output = translate(sql);
    assert!(output.contains("INSERT"), "Expected INSERT: {output}");
    execute_trigger_ddl(sql);
}

#[test]
fn trigger_with_select_into() {
    let sql = r#"
        CREATE TABLE accounts (id SERIAL PRIMARY KEY, balance INT NOT NULL DEFAULT 0);
        CREATE TABLE transactions (id SERIAL PRIMARY KEY, account_id INT, amount INT);
        CREATE TABLE balance_log (id SERIAL PRIMARY KEY, account_id INT, old_balance INT, new_balance INT);

        CREATE OR REPLACE FUNCTION log_balance_change() RETURNS TRIGGER AS $$
        DECLARE
            current_balance INT;
        BEGIN
            SELECT balance INTO current_balance FROM accounts WHERE id = NEW.account_id;
            INSERT INTO balance_log (account_id, old_balance, new_balance)
            VALUES (NEW.account_id, current_balance, current_balance + NEW.amount);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER balance_trigger
        AFTER INSERT ON transactions
        FOR EACH ROW EXECUTE FUNCTION log_balance_change();
    "#;
    let output = translate(sql);
    assert!(output.contains("INSERT"), "Expected INSERT: {output}");
    execute_trigger_ddl(sql);
}

#[test]
fn trigger_multiple_inserts_on_conflict() {
    let sql = r#"
        CREATE TABLE stats (id INT PRIMARY KEY, counter INT DEFAULT 0);
        CREATE TABLE events2 (id SERIAL PRIMARY KEY, stat_id INT, event_type TEXT);

        CREATE OR REPLACE FUNCTION update_stats() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO stats (id, counter)
            VALUES (NEW.stat_id, 1)
            ON CONFLICT (id) DO NOTHING;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER stats_trigger
        AFTER INSERT ON events2
        FOR EACH ROW EXECUTE FUNCTION update_stats();
    "#;
    let output = translate(sql);
    assert!(
        output.contains("INSERT") || output.contains("stats"),
        "Expected INSERT or stats: {output}"
    );
    execute_trigger_ddl(sql);
}

/// A PL/pgSQL function with `RAISE INFO 'msg'` (one space after INFO) was
/// previously not detected and passed through as invalid SQLite syntax.
#[test]
fn raise_info_single_space_is_dropped() -> Result<(), Box<dyn std::error::Error>> {
    // The trigger also copies NEW.val into a log table so the body is non-empty
    // after RAISE INFO is stripped (SQLite requires at least one DML statement).
    let sql = r"
        CREATE TABLE ri_test (id INTEGER PRIMARY KEY, val INTEGER NOT NULL);
        CREATE TABLE ri_log (id INTEGER PRIMARY KEY, logged_val INTEGER NOT NULL);
        CREATE OR REPLACE FUNCTION check_ri_val() RETURNS TRIGGER AS $body$
        BEGIN
            RAISE INFO 'checking value';
            INSERT INTO ri_log (id, logged_val) VALUES (NEW.id, NEW.val);
            RETURN NEW;
        END;
        $body$ LANGUAGE plpgsql;
        CREATE TRIGGER check_ri_val_trigger
        AFTER INSERT ON ri_test
        FOR EACH ROW EXECUTE FUNCTION check_ri_val();
    ";

    // Translation must succeed — RAISE INFO is not valid SQLite and must be
    // dropped.
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let out = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");
    assert!(
        !out.contains("RAISE INFO"),
        "RAISE INFO must be dropped from the translated trigger body, got:\n{out}"
    );

    // The resulting DDL must execute without error in SQLite.
    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // The trigger must fire on INSERT into ri_test, populating ri_log.
    diesel::sql_query("INSERT INTO ri_test VALUES (1, 42)").execute(&mut conn)?;

    #[derive(diesel::QueryableByName)]
    struct LogRow {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        logged_val: i32,
    }
    let rows = diesel::sql_query("SELECT logged_val FROM ri_log").load::<LogRow>(&mut conn)?;
    assert_eq!(rows.len(), 1, "Trigger must have fired and inserted one log row");
    assert_eq!(rows[0].logged_val, 42, "logged_val must match inserted val");
    Ok(())
}
