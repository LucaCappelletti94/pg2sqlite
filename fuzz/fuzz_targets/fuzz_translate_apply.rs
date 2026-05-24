//! Apply-time fuzz target.
//!
//! Feeds an `Arbitrary`-derived (options, sql) pair through
//! `Pg2Sqlite::sql().translate()` and then applies the translated
//! script to a fresh in-memory SQLite connection via `rusqlite`. The
//! contract under test:
//!
//!   If pg2sqlite parses the input as valid PostgreSQL and translates
//!   it without an error, the resulting SQLite script must at least
//!   parse cleanly in SQLite.
//!
//! Options are randomised per iteration so the full `with_*` matrix
//! is exercised (UUID Blob/Text, custom RLS suffix, sqlitegis, etc.).
//!
//! Lookup-class runtime errors ("no such table", "no such column",
//! type mismatches, constraint violations) are expected for
//! fuzz-generated SQL that references undeclared names, and are
//! filtered out. Syntax / parser errors on the SQLite side mean the
//! translator emitted malformed SQL - those panic so libfuzzer files
//! the input as a crash.

#![no_main]

use libfuzzer_sys::fuzz_target;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[derive(Debug, arbitrary::Arbitrary)]
struct FuzzInput {
    options: Pg2SqliteOptions,
    sql: String,
}

fuzz_target!(|input: FuzzInput| {
    // Slightly larger than the parse-only targets - apply paths
    // benefit from inputs that can fit both a CREATE TABLE and a few
    // statements that reference it.
    if input.sql.len() > 1024 {
        return;
    }

    // Parser / translator errors are expected and not signal here.
    // Only the apply step is under test.
    let Ok(parsed) = Pg2Sqlite::default().sql(&input.sql) else {
        return;
    };
    let Ok(stmts) = parsed.translate(&input.options) else {
        return;
    };

    let translated = stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n");

    let Ok(conn) = rusqlite::Connection::open_in_memory() else {
        return;
    };

    // Other error classes (no such table/column/function, type
    // mismatch, constraint violation) are user-side references that
    // wouldn't resolve in either backend; ignore.
    if let Err(err) = conn.execute_batch(&translated)
        && is_translator_bug(&err)
    {
        panic!(
            "translator emitted SQL that SQLite rejected with a syntax-class error: {err}\n\
             \n=== Options ===\n{:#?}\n\
             \n=== PostgreSQL input ===\n{}\n\
             \n=== Translated SQLite output ===\n{translated}\n",
            input.options, input.sql,
        );
    }
});

/// True for SQLite errors that indicate the translator produced
/// invalid SQL the SQLite parser refused to accept. Conservative -
/// we only flag the unambiguous syntax markers ("syntax error",
/// `near "..."`, "incomplete input"). Other runtime errors fall
/// through as not-a-translator-bug.
fn is_translator_bug(err: &rusqlite::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("syntax error")
        || msg.contains("near \"")
        || msg.contains("incomplete input")
        || msg.contains("unrecognized token")
        || msg.contains("malformed")
}
