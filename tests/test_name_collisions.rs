//! F18: two objects that leave with the same SQLite name.
//!
//! PostgreSQL gives every schema its own namespace and scopes trigger names to
//! a table. SQLite has one namespace for tables, views and indexes together,
//! and one for triggers across the whole database. So objects that were
//! distinct on the way in can arrive at the same name, and the emitted script
//! then fails at apply, or worse, silently keeps the first one when the create
//! carries `IF NOT EXISTS`.
//!
//! The check walks the emitted statements keeping the namespace SQLite itself
//! would keep, so a name freed by a `DROP` is available again. Every sequence
//! asserted here to translate was measured as valid SQLite before it was
//! written down, and the collisions were measured as the failures they are.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, TranslationOptions};
use rusqlite::Connection;

fn translate(sql: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
}

fn translate_err(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("translation should be refused")
        .to_string()
}

/// Applies every emitted statement, proving the script the crate hands over
/// runs as one script rather than merely parsing.
fn apply(sql: &str) {
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in translate(sql) {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted statement failed: {statement}: {error}"));
    }
}

/// A PL/pgSQL trigger body, since a `CREATE TRIGGER` needs one in the batch.
const TRIGGER_FUNCTION: &str = "CREATE FUNCTION f() RETURNS TRIGGER AS $$
     BEGIN NEW.n := 1; RETURN NEW; END; $$ LANGUAGE plpgsql;";

// --- collisions the flattening causes -------------------------------------

#[test]
fn two_tables_in_different_schemas_are_refused() {
    let error = translate_err(
        "CREATE SCHEMA x; CREATE SCHEMA y;
         CREATE TABLE x.t (id INT PRIMARY KEY);
         CREATE TABLE y.t (id INT PRIMARY KEY);",
    );
    assert!(error.contains("x.t"), "{error}");
    assert!(error.contains("y.t"), "{error}");
}

#[test]
fn an_unqualified_table_collides_with_a_schema_qualified_one() {
    let error = translate_err(
        "CREATE SCHEMA x;
         CREATE TABLE t (id INT PRIMARY KEY);
         CREATE TABLE x.t (id INT PRIMARY KEY);",
    );
    assert!(error.contains("x.t"), "{error}");
}

#[test]
fn two_views_in_different_schemas_are_refused() {
    let error = translate_err(
        "CREATE SCHEMA x; CREATE SCHEMA y;
         CREATE TABLE a (id INT PRIMARY KEY);
         CREATE VIEW x.v AS SELECT id FROM a;
         CREATE VIEW y.v AS SELECT id FROM a;",
    );
    assert!(error.contains("x.v"), "{error}");
    assert!(error.contains("y.v"), "{error}");
}

#[test]
fn two_indexes_in_different_schemas_are_refused() {
    let error = translate_err(
        "CREATE SCHEMA x; CREATE SCHEMA y;
         CREATE TABLE x.a (id INT PRIMARY KEY, n INT);
         CREATE TABLE y.b (id INT PRIMARY KEY, n INT);
         CREATE INDEX i ON x.a (n);
         CREATE INDEX i ON y.b (n);",
    );
    assert!(error.contains('i'), "{error}");
}

/// SQLite keeps tables and views in one namespace, so these two collide even
/// though they are different kinds of object.
#[test]
fn a_view_colliding_with_a_table_is_refused() {
    let error = translate_err(
        "CREATE SCHEMA x; CREATE SCHEMA y;
         CREATE TABLE x.t (id INT PRIMARY KEY);
         CREATE VIEW y.t AS SELECT id FROM x.t;",
    );
    assert!(error.contains("y.t"), "{error}");
}

#[test]
fn an_index_colliding_with_a_table_is_refused() {
    let error = translate_err(
        "CREATE SCHEMA x;
         CREATE TABLE a (id INT PRIMARY KEY, n INT);
         CREATE TABLE x.n (id INT PRIMARY KEY);
         CREATE INDEX n ON a (n);",
    );
    assert!(error.contains("x.n"), "{error}");
}

/// The one spelling SQLite does not raise on. It keeps the first table and
/// sends the second's rows into it, which is the silent case.
#[test]
fn if_not_exists_does_not_excuse_a_collision() {
    let error = translate_err(
        "CREATE SCHEMA x; CREATE SCHEMA y;
         CREATE TABLE IF NOT EXISTS x.t (id INT PRIMARY KEY);
         CREATE TABLE IF NOT EXISTS y.t (id INT PRIMARY KEY, n INT);",
    );
    assert!(error.contains("y.t"), "{error}");
}

#[test]
fn a_rename_onto_an_occupied_name_is_refused() {
    let error = translate_err(
        "CREATE SCHEMA x;
         CREATE TABLE a (id INT PRIMARY KEY);
         CREATE TABLE x.b (id INT PRIMARY KEY);
         ALTER TABLE a RENAME TO b;",
    );
    assert!(error.contains("x.b"), "{error}");
}

// --- collisions no schema causes ------------------------------------------

/// PostgreSQL scopes a trigger name to its table, SQLite to the database.
#[test]
fn the_same_trigger_name_on_two_tables_is_refused() {
    let error = translate_err(&format!(
        "CREATE TABLE a (id INT PRIMARY KEY, n INT);
         CREATE TABLE b (id INT PRIMARY KEY, n INT);
         {TRIGGER_FUNCTION}
         CREATE TRIGGER touch BEFORE INSERT ON a FOR EACH ROW EXECUTE FUNCTION f();
         CREATE TRIGGER touch BEFORE INSERT ON b FOR EACH ROW EXECUTE FUNCTION f();"
    ));
    assert!(error.contains("touch"), "{error}");
}

/// The backing table a row-security table generates is a name like any other.
#[test]
fn a_generated_backing_table_collides_with_a_declared_one() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE SCHEMA x;
             CREATE TABLE a (id INT PRIMARY KEY, owner TEXT);
             ALTER TABLE a ENABLE ROW LEVEL SECURITY;
             CREATE POLICY p ON a FOR SELECT USING (owner = 'me');
             CREATE TABLE x.a_rls (id INT PRIMARY KEY);",
        )
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default().with_rls_audit_table_name("audit"))
        .expect_err("translation should be refused")
        .to_string();
    assert!(error.contains("a_rls"), "{error}");
}

// --- sequences that must keep working -------------------------------------

#[test]
fn distinct_names_across_schemas_translate() {
    apply(
        "CREATE SCHEMA x; CREATE SCHEMA y;
         CREATE TABLE x.a (id INT PRIMARY KEY);
         CREATE TABLE y.b (id INT PRIMARY KEY);",
    );
}

#[test]
fn a_dropped_view_frees_its_name() {
    apply(
        "CREATE TABLE a (id INT PRIMARY KEY);
         CREATE VIEW v AS SELECT id FROM a;
         DROP VIEW v;
         CREATE VIEW v AS SELECT id + 1 AS id FROM a;",
    );
}

#[test]
fn a_dropped_index_frees_its_name() {
    apply(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT);
         CREATE INDEX i ON t (n);
         DROP INDEX i;
         CREATE INDEX i ON t (id);",
    );
}

/// Dropping a table takes its indexes with it, so the name is free again.
#[test]
fn a_dropped_table_frees_the_index_attached_to_it() {
    apply(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT);
         CREATE INDEX i ON t (n);
         DROP TABLE t;
         CREATE TABLE u (id INT PRIMARY KEY, n INT);
         CREATE INDEX i ON u (n);",
    );
}

#[test]
fn a_dropped_table_frees_the_trigger_attached_to_it() {
    apply(&format!(
        "CREATE TABLE a (id INT PRIMARY KEY, n INT);
         CREATE TABLE b (id INT PRIMARY KEY, n INT);
         {TRIGGER_FUNCTION}
         CREATE TRIGGER touch BEFORE INSERT ON a FOR EACH ROW EXECUTE FUNCTION f();
         DROP TABLE a;
         CREATE TRIGGER touch BEFORE INSERT ON b FOR EACH ROW EXECUTE FUNCTION f();"
    ));
}

#[test]
fn a_dropped_trigger_frees_its_name() {
    apply(&format!(
        "CREATE TABLE a (id INT PRIMARY KEY, n INT);
         CREATE TABLE b (id INT PRIMARY KEY, n INT);
         {TRIGGER_FUNCTION}
         CREATE TRIGGER touch BEFORE INSERT ON a FOR EACH ROW EXECUTE FUNCTION f();
         DROP TRIGGER touch ON a;
         CREATE TRIGGER touch BEFORE INSERT ON b FOR EACH ROW EXECUTE FUNCTION f();"
    ));
}

/// Triggers have their own namespace, so this is not a collision.
#[test]
fn a_trigger_may_carry_a_tables_name() {
    apply(&format!(
        "CREATE TABLE a (id INT PRIMARY KEY, n INT);
         CREATE TABLE touch (id INT PRIMARY KEY);
         {TRIGGER_FUNCTION}
         CREATE TRIGGER touch BEFORE INSERT ON a FOR EACH ROW EXECUTE FUNCTION f();"
    ));
}
