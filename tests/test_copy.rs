//! `COPY` must be refused, not discarded.
//!
//! `COPY ... FROM stdin` carries its rows inline in the migration file, so
//! dropping the statement loses data with no diagnostic. It used to sit in the
//! `unsupported_statement_patterns!()` catch-all alongside the other silent
//! drops.
//!
//! Translating it to multi-row `INSERT`s would be the better answer for a
//! migration translator, and it is not implemented, so every form is refused.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

const BASE: &str = "CREATE TABLE t (a INTEGER, b TEXT);";

/// Translates `pg` and returns the error message, failing if translation
/// succeeds.
fn rejection(pg: &str) -> String {
    let result = Pg2Sqlite::default()
        .sql(pg)
        .expect("the fixture must parse")
        .translate(&Pg2SqliteOptions::default())
        .map(pg2sqlite::script::Script::into_vec);

    match result {
        Ok(statements) => {
            panic!(
                "COPY must be refused rather than dropped, but translation produced {statements:?}"
            )
        }
        Err(error) => error.to_string(),
    }
}

/// The data-loss case. The rows live in the statement, so a silent drop
/// discards them.
#[test]
fn copy_from_stdin_is_rejected() {
    let error = rejection(&format!("{BASE} COPY t (a, b) FROM stdin;\n1\tx\n2\ty\n\\.\n"));
    assert!(error.contains("COPY"), "the error must name the statement, got: {error}");
}

/// A file source cannot be read by the translator and SQLite has no equivalent
/// statement.
#[test]
fn copy_from_a_file_is_rejected() {
    let error = rejection(&format!("{BASE} COPY t (a, b) FROM '/tmp/rows.csv';"));
    assert!(error.contains("COPY"), "the error must name the statement, got: {error}");
}

/// `COPY ... TO` exports rows. Dropping it silently produces no export at all.
#[test]
fn copy_to_stdout_is_rejected() {
    let error = rejection(&format!("{BASE} COPY t TO stdout;"));
    assert!(error.contains("COPY"), "the error must name the statement, got: {error}");
}

/// `COPY (SELECT ...) TO` is the query form of an export, and is refused for
/// the same reason.
#[test]
fn copy_from_a_query_is_rejected() {
    let error = rejection(&format!("{BASE} COPY (SELECT a FROM t) TO stdout;"));
    assert!(error.contains("COPY"), "the error must name the statement, got: {error}");
}
