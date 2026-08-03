//! Deferred foreign keys, which SQLite honours natively.
//!
//! Measured before implementing. SQLite accepts `DEFERRABLE` and `INITIALLY`
//! only on a foreign key clause, and answers `near "DEFERRABLE": syntax error`
//! on a `PRIMARY KEY`, `UNIQUE`, or `CHECK` constraint. It has no `ENFORCED`.
//! PostgreSQL 16 reads a bare `INITIALLY DEFERRED` as deferrable, reporting
//! `condeferrable=true`, while SQLite needs the keyword spelled out.

use diesel::{Connection, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

diesel::table! {
    /// Rows a deferred foreign key points at.
    parent (id) {
        /// Primary key.
        id -> Integer,
    }
}

diesel::table! {
    /// Rows carrying the deferred foreign key.
    child (id) {
        /// Primary key.
        id -> Integer,
        /// Reference to `parent.id`.
        pid -> Integer,
    }
}

const DEFERRED: &str = "
    CREATE TABLE parent (id INTEGER PRIMARY KEY);
    CREATE TABLE child (
        id INTEGER PRIMARY KEY,
        pid INTEGER NOT NULL REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED
    );
";

/// Translates `pg` and applies the emitted DDL to a fresh database with foreign
/// key enforcement switched on, which SQLite leaves off by default.
fn apply(pg: &str) -> SqliteConnection {
    let statements = Pg2Sqlite::default()
        .sql(pg)
        .expect("script should parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .unwrap_or_else(|error| panic!("script should translate: {error}"));

    let mut connection =
        SqliteConnection::establish(":memory:").expect("in-memory SQLite should open");
    // A pragma has no diesel DSL form, and SQLite ignores foreign keys without
    // this one.
    diesel::sql_query("PRAGMA foreign_keys = ON")
        .execute(&mut connection)
        .expect("pragma should apply");
    for statement in &statements {
        // Emitted DDL is the artifact under test, so it runs as text.
        diesel::sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted DDL failed: {error}\n{statement}"));
    }
    connection
}

/// Inserts a child row whose parent does not exist, creating the parent before
/// COMMIT when `repair_before_commit`. Returns whether the orphan was accepted
/// at statement time, and whether the transaction committed.
fn insert_orphan(connection: &mut SqliteConnection, repair_before_commit: bool) -> (bool, bool) {
    let mut orphan_accepted = false;
    let committed = connection.transaction::<_, diesel::result::Error, _>(|connection| {
        diesel::insert_into(child::table)
            .values((child::id.eq(1), child::pid.eq(99)))
            .execute(connection)?;
        orphan_accepted = true;
        if repair_before_commit {
            diesel::insert_into(parent::table).values(parent::id.eq(99)).execute(connection)?;
        }
        Ok(())
    });
    (orphan_accepted, committed.is_ok())
}

#[test]
fn a_deferred_foreign_key_tolerates_a_violation_inside_the_transaction() {
    let mut connection = apply(DEFERRED);
    let (orphan_accepted, committed) = insert_orphan(&mut connection, true);

    assert!(orphan_accepted, "a deferred key must accept the orphan at statement time");
    assert!(committed, "and commit it once the parent exists");
    assert_eq!(child::table.count().get_result(&mut connection), Ok(1i64));
}

/// The other half of deferral: it postpones the check, it does not drop it.
///
/// No row count afterwards. Measured: SQLite leaves the transaction open when
/// a deferred COMMIT fails, so the uncommitted row stays visible on this
/// connection until something rolls back. The COMMIT failing is the assertion
/// that matters, since nothing reached the database.
#[test]
fn a_deferred_foreign_key_still_fails_at_commit_when_the_parent_never_arrives() {
    let mut connection = apply(DEFERRED);
    let (orphan_accepted, committed) = insert_orphan(&mut connection, false);

    assert!(orphan_accepted, "the orphan is still accepted at statement time");
    assert!(!committed, "but COMMIT must reject it");
}

/// PostgreSQL reads a bare `INITIALLY DEFERRED` as deferrable, so the keyword
/// has to be written for SQLite, which rejects `INITIALLY` without it. The DDL
/// applying at all is what proves it, and the deferral proves it means the
/// same thing.
#[test]
fn initially_deferred_without_the_deferrable_keyword_still_defers() {
    let mut connection = apply(
        "CREATE TABLE parent (id INTEGER PRIMARY KEY);
         CREATE TABLE child (
             id INTEGER PRIMARY KEY,
             pid INTEGER NOT NULL REFERENCES parent(id) INITIALLY DEFERRED
         );",
    );
    let (orphan_accepted, committed) = insert_orphan(&mut connection, true);

    assert!(orphan_accepted, "a bare INITIALLY DEFERRED must defer");
    assert!(committed);
}

/// Guards the change from switching enforcement off rather than deferring it.
/// This passes before the change too.
#[test]
fn a_foreign_key_without_deferral_still_fails_at_statement_time() {
    let mut connection = apply(
        "CREATE TABLE parent (id INTEGER PRIMARY KEY);
         CREATE TABLE child (
             id INTEGER PRIMARY KEY,
             pid INTEGER NOT NULL REFERENCES parent(id)
         );",
    );
    let (orphan_accepted, committed) = insert_orphan(&mut connection, true);

    assert!(!orphan_accepted, "an immediate key rejects the orphan straight away");
    assert!(!committed);
}

/// Translates `pg` and returns the error, or the emitted SQL when it wrongly
/// succeeds.
fn reject(pg: &str) -> String {
    match Pg2Sqlite::default()
        .sql(pg)
        .expect("script should parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
    {
        Err(error) => error.to_string(),
        Ok(emitted) => panic!("expected a rejection, got: {}", emitted.join("\n")),
    }
}

/// SQLite carries deferrability only on a foreign key clause. The rejection
/// held before the change and has to survive it. Naming the constraint is new,
/// since one error used to cover every site and could name none of them.
#[test]
fn deferrability_outside_a_foreign_key_is_still_refused() {
    let unique = reject(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT, \
         CONSTRAINT uq UNIQUE (s) DEFERRABLE INITIALLY DEFERRED);",
    );
    assert!(unique.contains("UNIQUE"), "the error should name the constraint: {unique}");

    let primary = reject(
        "CREATE TABLE t (id INTEGER, s TEXT, \
         CONSTRAINT pk PRIMARY KEY (id) DEFERRABLE INITIALLY DEFERRED);",
    );
    assert!(primary.contains("PRIMARY KEY"), "the error should name the constraint: {primary}");
}

/// SQLite has no `ENFORCED` clause, so a foreign key carrying one is refused
/// even though its deferrability would translate.
#[test]
fn not_enforced_on_a_foreign_key_is_refused() {
    let error = reject(
        "CREATE TABLE parent (id INTEGER PRIMARY KEY);
         CREATE TABLE child (
             id INTEGER PRIMARY KEY,
             pid INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED NOT ENFORCED
         );",
    );
    assert!(error.contains("ENFORCED"), "the error should name the clause: {error}");
}
