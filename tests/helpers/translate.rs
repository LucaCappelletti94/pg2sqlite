//! Forward-translation helpers for integration tests that use default options.
//!
//! Include in a test binary with:
//!
//! ```rust,ignore
//! #[path = "helpers/translate.rs"]
//! mod translate_helpers;
//! use translate_helpers::translate_default as translate;
//! ```
//!
//! Using `#[path]` (rather than a sub-module declared in `helpers/mod.rs`)
//! ensures this file is compiled only in the binaries that include it,
//! so unused items never trigger dead-code warnings in other test binaries.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// Translates `pg` SQL with default options and joins all emitted statements
/// with newlines. Panics when parsing or translation fails.
///
/// Drop-in replacement for the boilerplate `fn translate` repeated across
/// test files.
// reason: Rust builds each integration test as its own binary, so a shared
// helper is genuinely unused in the binaries that need only the other one.
// Duplicating it per file is the alternative this module exists to remove.
#[allow(dead_code)]
pub fn translate_default(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Translates `pg` SQL with default options and returns the error message as a
/// string. Panics when the translation does not produce an error.
// reason: Rust builds each integration test as its own binary, so a shared
// helper is genuinely unused in the binaries that need only the other one.
// Duplicating it per file is the alternative this module exists to remove.
#[allow(dead_code)]
pub fn translate_default_err(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap_err()
        .to_string()
}
