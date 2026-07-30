//! Transaction control and `VACUUM` must emit SQLite that executes.
//!
//! Both used to be cloned verbatim by the passthrough arm, carrying
//! PostgreSQL-only clauses SQLite cannot parse. Verified on SQLite 3.51.1:
//! `BEGIN ISOLATION LEVEL SERIALIZABLE` is `near "ISOLATION": syntax error`,
//! `BEGIN READ ONLY` is `near "READ"`, `COMMIT AND CHAIN` is `near "AND"`,
//! `START TRANSACTION` is `near "START"` even with no modes at all, and
//! `VACUUM ANALYZE` is `unknown database ANALYZE`.
//!
//! A rejected `BEGIN` is worse than one bad statement, because the `COMMIT`
//! that follows then fails with "cannot commit, no transaction is active". One
//! unparseable clause takes the rest of the batch with it, which is why these
//! tests apply whole transactions rather than single statements.

use diesel::prelude::*;
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    warnings::{TranslationReport, TranslationWarning},
};

mod schema {
    diesel::table! {
        t (id) {
            id -> Integer,
        }
    }
}

fn report(pg: &str) -> TranslationReport {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate")
}

/// Applies every emitted statement in order. The emitted SQL is the artifact
/// under test, so it is applied as generated text.
fn apply(pg: &str) -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    for statement in report(pg).statements {
        diesel::sql_query(statement.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|error| panic!("emitted statement failed: {statement}: {error}"));
    }
    conn
}

fn translate_err(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("expected a translation error")
        .to_string()
}

const BASE: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY);";

/// A transaction carrying an isolation level executes, and the row it wrote
/// survives the commit. SQLite is at least as strict as any level PostgreSQL
/// can name, so dropping the clause cannot change what the transaction sees.
#[test]
fn a_transaction_with_an_isolation_level_commits() {
    let mut conn = apply(&format!(
        "{BASE}
         BEGIN ISOLATION LEVEL SERIALIZABLE;
         INSERT INTO t (id) VALUES (1);
         COMMIT;"
    ));

    assert_eq!(schema::t::table.count().get_result::<i64>(&mut conn).expect("count"), 1);
}

/// `START TRANSACTION` is a syntax error in SQLite whatever follows it, so the
/// spelling itself has to become `BEGIN`.
#[test]
fn start_transaction_becomes_begin() {
    let mut conn = apply(&format!(
        "{BASE}
         START TRANSACTION READ WRITE;
         INSERT INTO t (id) VALUES (1);
         COMMIT;"
    ));

    assert_eq!(schema::t::table.count().get_result::<i64>(&mut conn).expect("count"), 1);
}

/// `AND CHAIN` means commit and immediately open another transaction, so it is
/// translated rather than dropped: the second insert must be inside a
/// transaction that a later `ROLLBACK` can still undo.
#[test]
fn commit_and_chain_opens_the_next_transaction() {
    let mut conn = apply(&format!(
        "{BASE}
         BEGIN;
         INSERT INTO t (id) VALUES (1);
         COMMIT AND CHAIN;
         INSERT INTO t (id) VALUES (2);
         ROLLBACK;"
    ));

    let ids: Vec<i32> = schema::t::table.select(schema::t::id).load(&mut conn).expect("load");
    assert_eq!(ids, vec![1], "the chained transaction must be open for the rollback to undo it");
}

/// Same for `ROLLBACK AND CHAIN`.
#[test]
fn rollback_and_chain_opens_the_next_transaction() {
    let mut conn = apply(&format!(
        "{BASE}
         BEGIN;
         INSERT INTO t (id) VALUES (1);
         ROLLBACK AND CHAIN;
         INSERT INTO t (id) VALUES (2);
         ROLLBACK;"
    ));

    assert_eq!(
        schema::t::table.count().get_result::<i64>(&mut conn).expect("count"),
        0,
        "both transactions were rolled back"
    );
}

/// `BEGIN TRANSACTION` is SQLite's own spelling, so the keyword survives.
/// Guards the fix from stripping more than it has to.
#[test]
fn the_transaction_keyword_survives() {
    let sql = report(&format!("{BASE} BEGIN TRANSACTION; COMMIT;"))
        .statements
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");

    assert!(sql.contains("BEGIN TRANSACTION"), "expected the keyword to survive: {sql}");
    apply(&format!("{BASE} BEGIN TRANSACTION; INSERT INTO t (id) VALUES (1); COMMIT;"));
}

/// `BEGIN WORK` is the standard SQL spelling PostgreSQL also accepts, and
/// SQLite rejects it with `near "WORK": syntax error`, so that keyword has to
/// go while the transaction itself stays.
#[test]
fn the_work_keyword_is_dropped() {
    let mut conn =
        apply(&format!("{BASE} BEGIN WORK; INSERT INTO t (id) VALUES (1); COMMIT WORK;"));

    assert_eq!(schema::t::table.count().get_result::<i64>(&mut conn).expect("count"), 1);
}

/// A read-only transaction is refused rather than stripped. PostgreSQL rejects
/// a write inside one, so dropping the clause would let a write SQLite happily
/// performs replace an error the author was relying on.
#[test]
fn a_read_only_transaction_is_rejected() {
    let error = translate_err(&format!("{BASE} BEGIN READ ONLY; COMMIT;"));
    assert!(
        error.contains("query_only"),
        "expected the error to point at the SQLite equivalent, got {error}"
    );
}

/// Dropped transaction clauses are reported, since a caller reading the report
/// should learn the isolation level did not survive.
#[test]
fn dropping_a_transaction_clause_warns() {
    let warnings = report(&format!("{BASE} BEGIN ISOLATION LEVEL SERIALIZABLE; COMMIT;")).warnings;
    assert!(
        warnings.iter().any(|warning| {
            matches!(
                warning,
                TranslationWarning::LossyDrop { construct, .. } if *construct == "BEGIN"
            )
        }),
        "expected a LossyDrop naming BEGIN, got {warnings:?}"
    );
}

/// `VACUUM` takes no options in SQLite, so they are dropped and the bare
/// statement executes.
#[test]
fn vacuum_options_are_dropped() {
    apply(&format!("{BASE} VACUUM ANALYZE;"));
}

/// A `VACUUM` naming a table is the dangerous one: SQLite reads the name as a
/// schema and answers `unknown database t`, so the name has to go.
#[test]
fn vacuum_of_a_table_becomes_a_bare_vacuum() {
    let sql = report(&format!("{BASE} VACUUM t;"))
        .statements
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");

    assert!(!sql.contains("VACUUM t"), "the table name must not reach the output: {sql}");
    apply(&format!("{BASE} VACUUM t;"));
}

/// Both losses are reported.
#[test]
fn dropping_vacuum_options_warns() {
    let warnings = report(&format!("{BASE} VACUUM FULL t;")).warnings;
    assert!(
        warnings.iter().any(|warning| {
            matches!(
                warning,
                TranslationWarning::LossyDrop { construct, .. } if *construct == "VACUUM"
            )
        }),
        "expected a LossyDrop naming VACUUM, got {warnings:?}"
    );
}

/// A plain `VACUUM` loses nothing, so it must not warn. Guards the warning from
/// firing on every vacuum.
#[test]
fn a_bare_vacuum_does_not_warn() {
    let warnings = report(&format!("{BASE} VACUUM;")).warnings;
    assert!(warnings.is_empty(), "a bare VACUUM has nothing to report: {warnings:?}");
}
