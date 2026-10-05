//! Fuzz target for SQL translation.
//!
//! Feeds an `Arbitrary`-derived (options, sql) pair through the full
//! parsing and translation pipeline to find crashes or panics. The
//! options matrix is randomised per iteration so every `with_*`
//! toggle (UUID representation, RLS suffix, session variable
//! mappings, sqlitegis enable, etc.) gets exercised.

#![no_main]

use libfuzzer_sys::fuzz_target;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[derive(Debug, arbitrary::Arbitrary)]
struct FuzzInput {
    options: Pg2SqliteOptions,
    sql: String,
}

fuzz_target!(|input: FuzzInput| {
    if input.sql.len() > 500 {
        return;
    }
    if let Ok(parsed) = Pg2Sqlite::default().sql(&input.sql) {
        let _ = parsed.translate(&input.options);
    }
});
