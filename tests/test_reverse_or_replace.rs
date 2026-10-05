//! F23: the tables where `INSERT OR REPLACE` and `DO UPDATE` part company.
//!
//! SQLite's REPLACE deletes the conflicting rows and inserts a new one.
//! PostgreSQL's `ON CONFLICT (pk) DO UPDATE` updates the row in place. Where
//! nothing hangs off the delete the two agree, and the reverse direction keeps
//! translating those. Where something does, they differ in ways nobody would
//! notice until production, so those are refused.
//!
//! The three differences were measured on both engines with the same rows
//! before the fix:
//!
//! - triggers: SQLite fires the INSERT trigger, PostgreSQL fires the UPDATE one
//! - `ON DELETE CASCADE`: SQLite deletes the child, PostgreSQL keeps it
//! - a second unique constraint: SQLite deletes both conflicting rows,
//!   PostgreSQL raises `duplicate key value violates unique constraint`

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

fn schema(ddl: &str) -> ParserDB {
    Pg2Sqlite::default().sql(ddl).expect("parse").build_schema().expect("build")
}

fn reverse(ddl: &str, sqlite: &str) -> String {
    let statements = Pg2Sqlite::default()
        .reverse_sql(sqlite, &schema(ddl), &Pg2SqliteOptions::default())
        .expect("reverse translation");
    let sql = statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    Parser::parse_sql(&PostgreSqlDialect {}, &sql).expect("output parses as PostgreSQL");
    sql
}

fn reverse_err(ddl: &str, sqlite: &str) -> String {
    Pg2Sqlite::default()
        .reverse_sql(sqlite, &schema(ddl), &Pg2SqliteOptions::default())
        .expect_err("this table's REPLACE has no faithful PostgreSQL form")
        .to_string()
}

const PLAIN: &str = "CREATE TABLE u (id INT PRIMARY KEY, name TEXT, note TEXT);";

// --- the cases that still translate ---------------------------------------

#[test]
fn a_plain_table_still_reverses_to_an_upsert() {
    let pg = reverse(PLAIN, "INSERT OR REPLACE INTO u VALUES (1, 'x', 'y');");
    assert!(pg.contains("ON CONFLICT"), "{pg}");
    assert!(pg.contains("DO UPDATE"), "{pg}");
}

/// A child with no action is untouched by the delete on both engines, so
/// nothing diverges and the translation stands.
#[test]
fn a_non_cascading_child_does_not_block_the_upsert() {
    let ddl = "CREATE TABLE u (id INT PRIMARY KEY, name TEXT);
               CREATE TABLE c (id INT PRIMARY KEY, uid INT REFERENCES u(id));";
    let pg = reverse(ddl, "INSERT OR REPLACE INTO u VALUES (1, 'x');");
    assert!(pg.contains("DO UPDATE"), "{pg}");
}

/// The table's own outbound foreign key says nothing about what a delete of
/// its rows would cascade to.
#[test]
fn an_outbound_cascading_key_does_not_block_the_upsert() {
    let ddl = "CREATE TABLE p (id INT PRIMARY KEY);
               CREATE TABLE u (id INT PRIMARY KEY, pid INT REFERENCES p(id) ON DELETE CASCADE);";
    let pg = reverse(ddl, "INSERT OR REPLACE INTO u VALUES (1, 1);");
    assert!(pg.contains("DO UPDATE"), "{pg}");
}

#[test]
fn the_other_or_clauses_are_untouched() {
    let ignore = reverse(PLAIN, "INSERT OR IGNORE INTO u VALUES (1, 'x', 'y');");
    assert!(ignore.contains("DO NOTHING"), "{ignore}");
    let abort = reverse(PLAIN, "INSERT OR ABORT INTO u VALUES (1, 'x', 'y');");
    assert!(!abort.contains("ON CONFLICT"), "{abort}");
}

// --- triggers -------------------------------------------------------------

const TRIGGER_FUNCTION: &str = "CREATE FUNCTION f() RETURNS TRIGGER AS $$
     BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql;";

#[test]
fn a_table_with_a_delete_trigger_is_refused() {
    let ddl = format!(
        "CREATE TABLE u (id INT PRIMARY KEY, name TEXT);
         {TRIGGER_FUNCTION}
         CREATE TRIGGER td AFTER DELETE ON u FOR EACH ROW EXECUTE FUNCTION f();"
    );
    let error = reverse_err(&ddl, "INSERT OR REPLACE INTO u VALUES (1, 'x');");
    assert!(error.contains("trigger"), "{error}");
    assert!(error.contains("td"), "the message names the trigger: {error}");
}

/// SQLite fires this one and PostgreSQL does not, which the item's
/// delete-triggers-only reading would have let through.
#[test]
fn a_table_with_an_insert_trigger_is_refused() {
    let ddl = format!(
        "CREATE TABLE u (id INT PRIMARY KEY, name TEXT);
         {TRIGGER_FUNCTION}
         CREATE TRIGGER ti AFTER INSERT ON u FOR EACH ROW EXECUTE FUNCTION f();"
    );
    let error = reverse_err(&ddl, "INSERT OR REPLACE INTO u VALUES (1, 'x');");
    assert!(error.contains("ti"), "{error}");
}

/// PostgreSQL fires this one and SQLite does not.
#[test]
fn a_table_with_an_update_trigger_is_refused() {
    let ddl = format!(
        "CREATE TABLE u (id INT PRIMARY KEY, name TEXT);
         {TRIGGER_FUNCTION}
         CREATE TRIGGER tu AFTER UPDATE ON u FOR EACH ROW EXECUTE FUNCTION f();"
    );
    let error = reverse_err(&ddl, "INSERT OR REPLACE INTO u VALUES (1, 'x');");
    assert!(error.contains("tu"), "{error}");
}

/// A trigger on another table is not this table's business.
#[test]
fn a_trigger_on_another_table_does_not_block_the_upsert() {
    let ddl = format!(
        "CREATE TABLE u (id INT PRIMARY KEY, name TEXT);
         CREATE TABLE other (id INT PRIMARY KEY, n INT);
         {TRIGGER_FUNCTION}
         CREATE TRIGGER to_other AFTER DELETE ON other FOR EACH ROW EXECUTE FUNCTION f();"
    );
    let pg = reverse(&ddl, "INSERT OR REPLACE INTO u VALUES (1, 'x');");
    assert!(pg.contains("DO UPDATE"), "{pg}");
}

// --- inbound foreign keys that change a child ------------------------------

#[test]
fn a_cascading_child_is_refused() {
    let ddl = "CREATE TABLE u (id INT PRIMARY KEY, name TEXT);
               CREATE TABLE c (id INT PRIMARY KEY, uid INT REFERENCES u(id) ON DELETE CASCADE);";
    let error = reverse_err(ddl, "INSERT OR REPLACE INTO u VALUES (1, 'x');");
    assert!(error.contains('c'), "the message names the referencing table: {error}");
    assert!(error.contains("CASCADE"), "{error}");
}

#[test]
fn a_set_null_child_is_refused() {
    let ddl = "CREATE TABLE u (id INT PRIMARY KEY, name TEXT);
               CREATE TABLE c (id INT PRIMARY KEY, uid INT REFERENCES u(id) ON DELETE SET NULL);";
    let error = reverse_err(ddl, "INSERT OR REPLACE INTO u VALUES (1, 'x');");
    assert!(error.contains("SET NULL"), "{error}");
}

#[test]
fn a_set_default_child_is_refused() {
    let ddl = "CREATE TABLE u (id INT PRIMARY KEY, name TEXT);
               CREATE TABLE c (id INT PRIMARY KEY, uid INT DEFAULT 0 REFERENCES u(id) ON DELETE SET DEFAULT);";
    let error = reverse_err(ddl, "INSERT OR REPLACE INTO u VALUES (1, 'x');");
    assert!(error.contains("SET DEFAULT"), "{error}");
}

// --- a second unique constraint --------------------------------------------

#[test]
fn a_second_unique_column_is_refused() {
    let ddl = "CREATE TABLE u (id INT PRIMARY KEY, email TEXT UNIQUE, name TEXT);";
    let error = reverse_err(ddl, "INSERT OR REPLACE INTO u VALUES (1, 'a', 'x');");
    assert!(error.contains("unique"), "{error}");
}

#[test]
fn a_second_unique_table_constraint_is_refused() {
    let ddl = "CREATE TABLE u (id INT PRIMARY KEY, email TEXT, CONSTRAINT ue UNIQUE (email));";
    let error = reverse_err(ddl, "INSERT OR REPLACE INTO u VALUES (1, 'a');");
    assert!(error.contains("unique"), "{error}");
}
