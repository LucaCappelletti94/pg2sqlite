//! What a refused constraint clause says about itself.
//!
//! Each refusal here is forced, since SQLite cannot express the clause, so
//! the message is the whole deliverable and it has to be true about both
//! databases. Three were not: the deferrability refusal advised dropping a
//! deferral that is not droppable without changing when the check fires and
//! told the caller to move it to a foreign key that may not exist, the
//! foreign key `NOT ENFORCED` refusal never said the clause is not
//! PostgreSQL at all, and the `ALTER TABLE ADD CONSTRAINT` refusal pointed at
//! `CREATE TABLE` without saying that a circular pair can be declared there,
//! which measurement shows it can.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

/// The refusal `pg` earns.
fn refusal(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("expected a refusal")
        .to_string()
}

#[test]
fn deferred_uniqueness_says_what_each_database_does() {
    // PostgreSQL 17 accepts it and defers to commit, measured: two rows with
    // the same value coexist inside the transaction and the COMMIT fails.
    let message = refusal("CREATE TABLE t (u int UNIQUE DEFERRABLE INITIALLY DEFERRED);");
    assert!(message.contains("commit"), "the message names when PostgreSQL checks: {message}");
    assert!(
        !message.contains("Move the deferral to the foreign key"),
        "there is no foreign key to move it to: {message}"
    );
    assert!(
        message.to_lowercase().contains("statement"),
        "dropping the deferral moves the check to the statement, which the message says: \
         {message}"
    );
}

#[test]
fn a_deferred_primary_key_says_the_same() {
    let message = refusal(
        "CREATE TABLE t (k int, CONSTRAINT pk PRIMARY KEY (k) DEFERRABLE INITIALLY DEFERRED);",
    );
    assert!(message.contains("PRIMARY KEY"), "{message}");
    assert!(message.contains("commit"), "{message}");
}

#[test]
fn a_deferred_foreign_key_still_translates() {
    // The refusal is for the constraints SQLite cannot defer; a foreign key
    // it can, and that has to keep working.
    let statements = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE p (id int primary key);
             CREATE TABLE c (id int primary key, pid int REFERENCES p(id) DEFERRABLE INITIALLY DEFERRED);",
        )
        .expect("fixture parses")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("a deferred foreign key is translatable");
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted statement failed: {error}\n{statement}"));
    }
    connection
        .execute_batch("BEGIN; INSERT INTO c VALUES (10, 1); INSERT INTO p VALUES (1); COMMIT;")
        .expect("the child may precede the parent inside one transaction");
}

#[test]
fn the_foreign_key_not_enforced_refusal_names_the_dialect() {
    // PostgreSQL answers `syntax error at or near "ENFORCED"`, so the input
    // is not PostgreSQL, which is what the CHECK sibling already says.
    let message = refusal(
        "CREATE TABLE p (id int primary key);
         CREATE TABLE c (id int, FOREIGN KEY (id) REFERENCES p(id) NOT ENFORCED);",
    );
    assert!(message.contains("MySQL"), "{message}");
    assert!(message.contains("PostgreSQL"), "{message}");
}

#[test]
fn adding_a_foreign_key_says_why_a_circular_pair_has_no_form() {
    // Measured: PostgreSQL refuses a forward reference too, `relation "fb"
    // does not exist`, so a circular pair is expressible there only through
    // ALTER TABLE, which SQLite has no form of. The message has to say that
    // rather than point at CREATE TABLE as though an order existed.
    let message = refusal(
        "CREATE TABLE a (id int primary key, aid int);
         CREATE TABLE b (id int primary key, bid int);
         ALTER TABLE a ADD CONSTRAINT a_fk FOREIGN KEY (aid) REFERENCES b(id) DEFERRABLE INITIALLY DEFERRED;",
    );
    assert!(message.contains("CREATE TABLE"), "{message}");
    assert!(
        message.to_lowercase().contains("circular")
            || message.to_lowercase().contains("each other"),
        "the message names the pair that has no order: {message}"
    );
    assert!(
        message.to_lowercase().contains("trigger"),
        "and what is left, which is a trigger: {message}"
    );
}

#[test]
fn a_forward_reference_is_refused_as_postgresql_refuses_it() {
    // `relation "b" does not exist` on the server; the replica says the same
    // thing rather than emitting a table whose target may never arrive.
    let message = refusal(
        "CREATE TABLE a (id int primary key, bid int REFERENCES b(id));
         CREATE TABLE b (id int primary key);",
    );
    assert!(message.contains('b'), "{message}");
}
