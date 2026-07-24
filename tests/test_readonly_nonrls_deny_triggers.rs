//! Synchronous write denial for read-only non-RLS tables under a session role.
//!
//! A table that is selectable but not writable and carries no RLS policy is
//! emitted as a plain table plus `BEFORE INSERT`/`UPDATE`/`DELETE` deny
//! triggers that `RAISE(ABORT)`, so interactive writes fail at the statement
//! rather than deferring to a server-side catalog rejection. Authoritative
//! changeset applies must disable triggers (`SQLITE_DBCONFIG_ENABLE_TRIGGER`),
//! pinned by the triggers-disabled test; `rusqlite` is used there because
//! `diesel` does not expose `sqlite3_db_config`.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, TranslationOptions};

diesel::table! {
    /// The read-only reference table.
    orders (id) {
        /// Primary key.
        id -> Integer,
        /// Item label.
        item -> Text,
    }
}

#[derive(Insertable)]
#[diesel(table_name = orders)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct NewOrder {
    id: i32,
    item: String,
}

/// Schema with a read-only non-RLS table (`orders`), a writable non-RLS table
/// (`widgets`), and a non-selectable table (`audit_logs`).
const SCHEMA: &str = "\
CREATE ROLE app_user;
CREATE TABLE orders (id INTEGER PRIMARY KEY, item TEXT NOT NULL);
GRANT SELECT ON orders TO app_user;
CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
GRANT SELECT, INSERT, UPDATE, DELETE ON widgets TO app_user;
CREATE TABLE audit_logs (id INTEGER PRIMARY KEY, msg TEXT NOT NULL);
";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_session_user_role("app_user")
}

fn translate(schema: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(schema)
        .expect("parse")
        .translate(&options())
        .expect("translate")
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn apply(schema: &str) -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    for stmt in translate(schema) {
        diesel::sql_query(stmt).execute(&mut conn).expect("apply statement");
    }
    conn
}

#[test]
fn readonly_nonrls_select_works_and_insert_fails() {
    let mut conn = apply(SCHEMA);

    // SELECT is not blocked.
    let count: i64 = orders::table.count().get_result(&mut conn).expect("select works");
    assert_eq!(count, 0);

    // INSERT fails synchronously at the statement.
    let result = diesel::insert_into(orders::table)
        .values(NewOrder { id: 1, item: "widget".to_string() })
        .execute(&mut conn);
    assert!(result.is_err(), "INSERT into read-only orders should fail, got {result:?}");
    assert!(
        format!("{:?}", result.unwrap_err()).contains("read-only"),
        "deny message should mention read-only"
    );
}

#[test]
fn writable_nonrls_table_has_no_deny_triggers() {
    let joined = translate(SCHEMA).join("\n");
    assert!(
        !joined.contains("widgets__readonly"),
        "writable table must not get deny triggers, got:\n{joined}"
    );
}

#[test]
fn non_selectable_table_is_omitted_without_triggers() {
    let joined = translate(SCHEMA).join("\n");
    assert!(!joined.contains("audit_logs"), "non-selectable table must be omitted, got:\n{joined}");
}

#[test]
fn deny_trigger_names_are_deterministic() {
    let joined = translate(SCHEMA).join("\n");
    for (name, event) in [
        ("orders__readonly_insert", "BEFORE INSERT"),
        ("orders__readonly_update", "BEFORE UPDATE"),
        ("orders__readonly_delete", "BEFORE DELETE"),
    ] {
        assert!(joined.contains(name), "expected trigger {name}, got:\n{joined}");
        assert!(joined.contains(event), "expected {event} trigger, got:\n{joined}");
    }
    assert!(joined.contains("RAISE(ABORT"), "deny triggers must RAISE(ABORT), got:\n{joined}");
}

#[test]
fn reserved_trigger_name_collision_is_rejected() {
    let schema = "\
CREATE ROLE app_user;
CREATE TABLE orders (id INTEGER PRIMARY KEY, item TEXT NOT NULL);
GRANT SELECT ON orders TO app_user;
CREATE TABLE \"orders__readonly_insert\" (x INTEGER);
";
    let result = Pg2Sqlite::default().sql(schema).expect("parse").translate(&options());
    let err = result.expect_err("collision with reserved trigger name must error");
    let message = err.to_string();
    assert!(
        message.contains("orders__readonly_insert"),
        "error should name the colliding trigger, got: {message}"
    );
}

#[test]
fn reserved_index_name_collision_is_rejected() {
    let schema = "\
CREATE ROLE app_user;
CREATE TABLE orders (id INTEGER PRIMARY KEY, item TEXT NOT NULL);
GRANT SELECT ON orders TO app_user;
CREATE INDEX \"orders__readonly_update\" ON orders (item);
";
    let result = Pg2Sqlite::default().sql(schema).expect("parse").translate(&options());
    let err = result.expect_err("collision with a reserved index name must error");
    let message = err.to_string();
    assert!(
        message.contains("orders__readonly_update"),
        "error should name the colliding index, got: {message}"
    );
}

#[test]
fn reserved_name_collision_is_case_insensitive() {
    // SQLite resolves object names case-insensitively, so a declared object
    // whose name differs only in case from a generated deny trigger still
    // collides at apply time and must be rejected up front.
    let schema = "\
CREATE ROLE app_user;
CREATE TABLE orders (id INTEGER PRIMARY KEY, item TEXT NOT NULL);
GRANT SELECT ON orders TO app_user;
CREATE INDEX \"ORDERS__READONLY_INSERT\" ON orders (item);
";
    let result = Pg2Sqlite::default().sql(schema).expect("parse").translate(&options());
    assert!(result.is_err(), "case-differing name must still collide, got: {result:?}");
}

#[test]
fn reserved_view_name_collision_is_rejected() {
    let schema = "\
CREATE ROLE app_user;
CREATE TABLE orders (id INTEGER PRIMARY KEY, item TEXT NOT NULL);
GRANT SELECT ON orders TO app_user;
CREATE VIEW \"orders__readonly_delete\" AS SELECT id FROM orders;
";
    let result = Pg2Sqlite::default().sql(schema).expect("parse").translate(&options());
    let err = result.expect_err("collision with a reserved view name must error");
    let message = err.to_string();
    assert!(
        message.contains("orders__readonly_delete"),
        "error should name the colliding view, got: {message}"
    );
}

#[test]
fn reserved_trigger_declaration_collision_is_rejected() {
    let schema = "\
CREATE ROLE app_user;
CREATE TABLE orders (id INTEGER PRIMARY KEY, item TEXT NOT NULL);
GRANT SELECT ON orders TO app_user;
CREATE TRIGGER \"orders__readonly_update\" BEFORE UPDATE ON orders FOR EACH ROW EXECUTE FUNCTION noop();
";
    let result = Pg2Sqlite::default().sql(schema).expect("parse").translate(&options());
    let err = result.expect_err("collision with a reserved trigger name must error");
    let message = err.to_string();
    assert!(
        message.contains("orders__readonly_update"),
        "error should name the colliding trigger, got: {message}"
    );
}

#[test]
fn custom_suffix_renames_triggers_and_dodges_collision() {
    // A schema that collides with the default marker translates cleanly once
    // the marker is reconfigured, mirroring the RLS `with_rls_table_suffix`
    // escape hatch.
    let schema = "\
CREATE ROLE app_user;
CREATE TABLE orders (id INTEGER PRIMARY KEY, item TEXT NOT NULL);
GRANT SELECT ON orders TO app_user;
CREATE TABLE \"orders__readonly_insert\" (x INTEGER);
";
    let opts = options().with_readonly_deny_trigger_suffix("__ro");
    let joined = Pg2Sqlite::default()
        .sql(schema)
        .expect("parse")
        .translate(&opts)
        .expect("custom marker avoids the default collision")
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    for name in ["orders__ro_insert", "orders__ro_update", "orders__ro_delete"] {
        assert!(joined.contains(name), "expected trigger {name}, got:\n{joined}");
    }
    assert!(
        !joined.contains("orders__readonly_insert BEFORE"),
        "default marker must not be used, got:\n{joined}"
    );
}

#[test]
fn deny_triggers_are_inert_when_disabled_and_block_when_enabled() {
    // rusqlite: diesel does not expose sqlite3_db_config, needed to toggle
    // SQLITE_DBCONFIG_ENABLE_TRIGGER and pin the apply contract.
    use rusqlite::{Connection, config::DbConfig};

    let conn = Connection::open_in_memory().expect("open");
    let batch = format!("{};", translate(SCHEMA).join(";\n"));
    conn.execute_batch(&batch).expect("apply schema");

    // With triggers disabled, the apply-path INSERT succeeds.
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false).expect("disable triggers");
    conn.execute("INSERT INTO orders (id, item) VALUES (1, 'seed')", [])
        .expect("insert succeeds while triggers disabled");

    // Re-enable triggers: interactive writes are denied again.
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, true).expect("enable triggers");

    let count: i64 = conn.query_row("SELECT COUNT(*) FROM orders", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1, "seeded row is visible via SELECT");

    assert!(
        conn.execute("INSERT INTO orders (id, item) VALUES (2, 'x')", []).is_err(),
        "INSERT must be denied when triggers are enabled"
    );
    assert!(
        conn.execute("UPDATE orders SET item = 'x' WHERE id = 1", []).is_err(),
        "UPDATE must be denied when triggers are enabled"
    );
    assert!(
        conn.execute("DELETE FROM orders WHERE id = 1", []).is_err(),
        "DELETE must be denied when triggers are enabled"
    );

    let after: i64 = conn.query_row("SELECT COUNT(*) FROM orders", [], |r| r.get(0)).unwrap();
    assert_eq!(after, 1, "denied writes must not change the table");
}
