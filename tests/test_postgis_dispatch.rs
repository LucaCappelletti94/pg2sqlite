//! Step 3 (always-on): PostGIS function dispatch logic when
//! `Pg2SqliteOptions::enable_geolite` is on or off.
//!
//! These tests cover translation behavior only, no SQLite execution, so
//! they don't require the `geolite` cargo feature. End-to-end execution
//! against the geolite extension lives in `test_postgis_diesel.rs`.

use pg2sqlite::{
    impls::translator_impls::postgis,
    pg2sqlite::Pg2Sqlite,
    prelude::{Pg2SqliteOptions, TranslationOptions},
};

#[test]
fn st_point_passthrough_when_geolite_disabled() {
    // Backward compat: without `enable_geolite`, any ST_* call falls into the
    // existing PassThrough fallback so legacy users aren't broken.
    let translated = Pg2Sqlite::default()
        .sql("SELECT ST_Point(0, 0) AS p;")
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("should pass through ST_Point untouched when geolite is disabled");
    let joined = translated.join("\n").to_ascii_uppercase();
    assert!(joined.contains("ST_POINT"), "expected verbatim ST_Point passthrough, got: {joined}");
}

#[test]
fn st_point_passthrough_when_geolite_enabled_and_arity_matches() {
    let opts = Pg2SqliteOptions::default().with_geolite_enabled();
    let translated = Pg2Sqlite::default()
        .sql("SELECT ST_Point(0, 0) AS p;")
        .expect("parse")
        .translate_to_sql(&opts)
        .expect("ST_Point/2 is in geolite catalog and should pass through");
    let joined = translated.join("\n").to_ascii_uppercase();
    assert!(joined.contains("ST_POINT"), "got: {joined}");
}

#[test]
fn st_point_wrong_arity_errors_when_geolite_enabled() {
    let opts = Pg2SqliteOptions::default().with_geolite_enabled();
    let result = Pg2Sqlite::default()
        .sql("SELECT ST_Point(1) AS x;")
        .expect("parse")
        .translate_to_sql(&opts);
    let err = result.expect_err("ST_Point/1 is not in geolite catalog, must error");
    let msg = format!("{err:?}");
    assert!(
        msg.to_ascii_lowercase().contains("st_point") && msg.contains('1'),
        "error should name the function and arity, got: {msg}"
    );
}

#[test]
fn st_transform_errors_when_geolite_enabled() {
    // ST_Transform is a real PostGIS function but is NOT in geolite's catalog
    // (no SRID reprojection yet). Hard error so users notice setup gaps.
    let opts = Pg2SqliteOptions::default().with_geolite_enabled();
    let result = Pg2Sqlite::default()
        .sql("SELECT ST_Transform(geom, 4326) AS x FROM t;")
        .expect("parse")
        .translate_to_sql(&opts);
    let err = result.expect_err("ST_Transform must error when geolite is enabled");
    assert!(
        format!("{err:?}").to_ascii_lowercase().contains("st_transform"),
        "error should name the function, got: {err:?}"
    );
}

#[test]
fn catalog_lookup_finds_st_point_with_known_arities() {
    assert!(postgis::is_geolite_function("st_point", 2));
    assert!(postgis::is_geolite_function("st_point", 3));
    assert!(!postgis::is_geolite_function("st_point", 1));
    assert!(!postgis::is_geolite_function("st_point", 4));
}

#[test]
fn catalog_lookup_is_case_insensitive_on_supplied_name() {
    // Callers may pass mixed case from the parser. Catalog stores lowercase,
    // and lookups should treat the input as case-insensitive.
    assert!(postgis::is_geolite_function("ST_Point", 2));
    assert!(postgis::is_geolite_function("st_POINT", 2));
}

#[test]
fn catalog_lookup_rejects_unknown_names() {
    assert!(!postgis::is_geolite_function("st_transform", 2));
    assert!(!postgis::is_geolite_function("st_simplify", 2));
    assert!(!postgis::is_geolite_function("now", 0)); // not PostGIS
}

#[test]
fn catalog_includes_spatial_index_helpers() {
    // SQLite-side DDL helpers from geolite's DIRECT_ONLY catalog.
    assert!(postgis::is_geolite_function("createspatialindex", 2));
    assert!(postgis::is_geolite_function("dropspatialindex", 2));
}
