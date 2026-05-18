//! Step 3 (geolite-gated): catalog parity + end-to-end execution of every
//! `ST_*` smoke statement against a Diesel `SqliteConnection` with geolite's
//! extension loaded.
//!
//! Run with: `cargo test --features geolite --test test_postgis_diesel`

#![cfg(feature = "geolite")]

mod helpers;

use diesel::{RunQueryDsl, sql_query};
use geolite_core::function_catalog::{
    SQLITE_DETERMINISTIC_FUNCTIONS, SQLITE_DIRECT_ONLY_FUNCTIONS,
};
use helpers::geolite::geolite_connection;
use pg2sqlite::{
    impls::translator_impls::postgis,
    pg2sqlite::Pg2Sqlite,
    prelude::{Pg2SqliteOptions, TranslationOptions},
};

#[test]
fn pg2sqlite_catalog_covers_every_geolite_deterministic_function() {
    let mut missing = Vec::new();
    for spec in SQLITE_DETERMINISTIC_FUNCTIONS {
        if !postgis::is_geolite_function(&spec.name.to_ascii_lowercase(), spec.n_arg) {
            missing.push(format!("{}/{}", spec.name, spec.n_arg));
        }
    }
    assert!(
        missing.is_empty(),
        "pg2sqlite's PostGIS catalog is missing {} geolite deterministic entries:\n{}",
        missing.len(),
        missing.join(", ")
    );
}

#[test]
fn pg2sqlite_catalog_covers_every_geolite_direct_only_function() {
    let mut missing = Vec::new();
    for spec in SQLITE_DIRECT_ONLY_FUNCTIONS {
        if !postgis::is_geolite_function(&spec.name.to_ascii_lowercase(), spec.n_arg) {
            missing.push(format!("{}/{}", spec.name, spec.n_arg));
        }
    }
    assert!(
        missing.is_empty(),
        "pg2sqlite's PostGIS catalog is missing {} geolite direct-only entries:\n{}",
        missing.len(),
        missing.join(", ")
    );
}

#[test]
fn every_geolite_smoke_sql_translates_and_executes() {
    let mut conn = geolite_connection();
    let opts = Pg2SqliteOptions::default().with_geolite_enabled();

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
