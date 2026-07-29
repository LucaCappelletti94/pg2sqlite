//! `COPY` must be refused, not discarded.
//!
//! `COPY ... FROM stdin` carries its rows inline in the migration file, so
//! dropping the statement loses data with no diagnostic. It used to sit in the
//! `unsupported_statement_patterns!()` catch-all alongside the other silent
//! drops.
//!
//! Translating it to multi-row `INSERT`s would be the better answer for a
//! migration translator, and it is blocked upstream rather than merely
//! unimplemented. `Parser::parse_tab_value` flattens the payload into a
//! `Vec<Option<String>>` that carries no row boundaries, because a tab and a
//! newline both simply push a value, and a `\N` null desynchronises the list by
//! leaving a phantom empty field behind. The row structure therefore cannot be
//! recovered from the AST, and the raw text is already consumed. The last test
//! here pins that defect so this decision is revisited if upstream fixes it.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::{
    ast::{CopySource, Statement},
    dialect::PostgreSqlDialect,
    parser::Parser,
};

const BASE: &str = "CREATE TABLE t (a INTEGER, b TEXT);";

/// Translates `pg` and returns the error message, failing if translation
/// succeeds.
fn rejection(pg: &str) -> String {
    let result = Pg2Sqlite::default()
        .sql(pg)
        .expect("the fixture must parse")
        .translate(&Pg2SqliteOptions::default());

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

/// Pins the upstream defect that blocks translating `COPY ... FROM stdin` into
/// `INSERT`s, so the choice is revisited rather than forgotten if it is fixed.
///
/// Two rows of two fields should yield four values. Instead the payload arrives
/// with a leading empty string and, once a `\N` appears, an extra phantom empty
/// field, so the flat list cannot be chunked back into rows.
#[test]
fn sqlparser_still_flattens_copy_payload_rows() {
    let sql = "COPY t (a, b) FROM stdin;\n1\t\\N\n2\ty\n\\.\n";
    let statements = Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("the fixture must parse");

    let Some(Statement::Copy { values, source, .. }) = statements.first() else {
        panic!("expected a COPY statement, got {statements:?}");
    };
    let CopySource::Table { columns, .. } = source else {
        panic!("expected a table source");
    };

    assert_eq!(columns.len(), 2, "the fixture names two columns");
    assert_eq!(
        values.len(),
        6,
        "two rows of two fields should be four values. If this now reports 4, upstream has \
         restored the row structure and COPY FROM stdin can become INSERTs: revisit plan item R9. \
         Got {values:?}"
    );
    assert_eq!(
        values.first(),
        Some(&Some(String::new())),
        "the payload still begins with a phantom empty field"
    );
}
