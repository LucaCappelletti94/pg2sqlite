//! F22: the PostgreSQL-only index clauses the translation drops in silence.
//!
//! `CreateIndex::translate` drops four things a `PostgreSqlDialect` parse can
//! actually deliver, and said nothing about any of them. Under D2 a drop that
//! cannot change a result is still reportable, which is what the operator-class
//! site next door already does.
//!
//! The index still applies and still enforces what it enforced, so these are
//! warnings rather than refusals. The tests assert the emitted DDL runs as well
//! as the warning being raised, since a warning about SQL nobody executes is
//! worth nothing.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    warnings::TranslationWarning,
};
use rusqlite::Connection;

const FIXTURE: &str = "CREATE TABLE ic (a INT, b INT, c TEXT);";

/// Translates, runs every emitted statement, and returns the warnings.
fn warnings_for(ddl: &str) -> Vec<TranslationWarning> {
    let report = Pg2Sqlite::default()
        .sql(&format!("{FIXTURE}\n{ddl}"))
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate");

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &report.statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted statement failed: {statement}: {error}"));
    }
    report.warnings
}

fn downgrade(warnings: &[TranslationWarning], construct: &str) -> (String, String, String) {
    for warning in warnings {
        if let TranslationWarning::LossyDowngrade { construct: kind, from, to, reason, .. } =
            warning
            && kind == construct
        {
            return (from.clone(), to.clone(), reason.clone());
        }
    }
    panic!("no LossyDowngrade for {construct} in {warnings:?}");
}

fn drop_reason(warnings: &[TranslationWarning], construct: &str) -> String {
    for warning in warnings {
        if let TranslationWarning::LossyDrop { construct: kind, reason } = warning
            && kind == construct
        {
            return reason.clone();
        }
    }
    panic!("no LossyDrop for {construct} in {warnings:?}");
}

#[test]
fn included_columns_are_named_in_a_warning() {
    let warnings = warnings_for("CREATE INDEX i ON ic (a) INCLUDE (b);");
    let (from, to, reason) = downgrade(&warnings, "index INCLUDE");
    assert!(from.contains('b'), "the warning names the dropped column: {from}");
    assert!(to.contains('a'), "and what was emitted instead: {to}");
    assert!(reason.contains("index-only"), "{reason}");
}

#[test]
fn several_included_columns_are_all_named() {
    let warnings = warnings_for("CREATE UNIQUE INDEX i ON ic (a) INCLUDE (b, c);");
    let (from, _, _) = downgrade(&warnings, "index INCLUDE");
    assert!(from.contains('b') && from.contains('c'), "{from}");
}

/// An INCLUDE column is payload, not key, so the uniqueness the index enforces
/// is unchanged by dropping it. The emitted index must still refuse a
/// duplicate.
#[test]
fn dropping_the_payload_leaves_the_uniqueness_alone() {
    let statements = Pg2Sqlite::default()
        .sql(&format!("{FIXTURE}\nCREATE UNIQUE INDEX i ON ic (a) INCLUDE (b);"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate");
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection.execute_batch(&format!("{statement};")).expect("emitted statement");
    }
    connection.execute_batch("INSERT INTO ic (a, b) VALUES (1, 1);").expect("first row");
    let second = connection.execute_batch("INSERT INTO ic (a, b) VALUES (1, 2);");
    assert!(second.is_err(), "the unique index must still refuse a duplicate key");
}

#[test]
fn a_hash_index_method_is_named_in_a_warning() {
    let warnings = warnings_for("CREATE INDEX i ON ic USING hash (a);");
    let (from, to, _) = downgrade(&warnings, "index method");
    assert!(from.to_lowercase().contains("hash"), "{from}");
    assert!(to.to_lowercase().contains("tree") || to.contains("default"), "{to}");
}

#[test]
fn a_brin_index_method_is_named_in_a_warning() {
    let warnings = warnings_for("CREATE INDEX i ON ic USING brin (a);");
    let (from, _, _) = downgrade(&warnings, "index method");
    assert!(from.to_lowercase().contains("brin"), "{from}");
}

#[test]
fn concurrently_is_reported_as_dropped() {
    let warnings = warnings_for("CREATE INDEX CONCURRENTLY i ON ic (a);");
    let reason = drop_reason(&warnings, "CREATE INDEX CONCURRENTLY");
    assert!(reason.contains("lock"), "the reason is about locking, not the index: {reason}");
}

#[test]
fn storage_parameters_are_reported_as_dropped() {
    let warnings = warnings_for("CREATE INDEX i ON ic (a) WITH (fillfactor = 70);");
    let reason = drop_reason(&warnings, "index storage parameters");
    assert!(reason.contains("fillfactor") || reason.contains("storage"), "{reason}");
}

/// The default shape must stay quiet, or the report becomes noise.
#[test]
fn a_plain_index_warns_about_nothing() {
    let warnings = warnings_for("CREATE INDEX i ON ic (a);");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_partial_unique_index_warns_about_nothing() {
    let warnings = warnings_for("CREATE UNIQUE INDEX i ON ic (a) WHERE b > 0;");
    assert!(warnings.is_empty(), "{warnings:?}");
}

/// Two clauses on one index raise two warnings rather than the first only.
#[test]
fn every_dropped_clause_gets_its_own_warning() {
    let warnings =
        warnings_for("CREATE INDEX CONCURRENTLY i ON ic USING hash (a) WITH (fillfactor = 70);");
    assert_eq!(warnings.len(), 3, "{warnings:?}");
}
