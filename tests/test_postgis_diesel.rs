//! Step 3 (sqlitegis-gated): catalog parity + end-to-end execution of every
//! `ST_*` smoke statement against a Diesel `SqliteConnection` with the
//! SQLiteGIS extension loaded.
//!
//! Run with: `cargo test --features sqlitegis --test test_postgis_diesel`

#![cfg(feature = "sqlitegis")]

mod helpers;

use diesel::{RunQueryDsl, sql_query};
use helpers::sqlitegis::sqlitegis_connection;
use pg2sqlite::{pg2sqlite::Pg2Sqlite, prelude::Pg2SqliteOptions};
use sqlitegis::core::function_catalog::SQLITE_DETERMINISTIC_FUNCTIONS;

#[test]
fn every_sqlitegis_smoke_sql_translates_and_executes() {
    let mut conn = sqlitegis_connection();
    let opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();

    // DIRECT-ONLY helpers (CreateSpatialIndex / DropSpatialIndex) need a real
    // table to bind to; skip them in this generic smoke loop (Step 4 covers
    // them properly via GiST index routing).
    let mut failures: Vec<String> = Vec::new();
    for spec in SQLITE_DETERMINISTIC_FUNCTIONS {
        let pg_sql = format!("{};", spec.smoke_sql);
        let translated =
            match Pg2Sqlite::default().sql(&pg_sql).and_then(|t| t.translate_to_sql(&opts)) {
                Ok(stmts) => stmts.join("; "),
                Err(e) => {
                    failures.push(format!("{}/{}: translate failed: {e:?}", spec.name, spec.n_arg));
                    continue;
                }
            };
        if let Err(e) = sql_query(&translated).execute(&mut conn) {
            failures.push(format!(
                "{}/{}: execute failed for `{translated}`: {e:?}",
                spec.name, spec.n_arg
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} smoke failures across {} catalog entries:\n  {}",
        failures.len(),
        SQLITE_DETERMINISTIC_FUNCTIONS.len(),
        failures.join("\n  ")
    );
}
