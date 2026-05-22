//! Step 4 (always-on): GiST indexes on geometry/geography columns translate
//! into `SELECT CreateSpatialIndex('tbl', 'col')` calls that geolite
//! executes at runtime to set up an rtree shadow table.
//!
//! These tests cover the dispatch behavior only, no SQLite execution.
//! The end-to-end Diesel variant lives in `test_postgis_gist_diesel.rs`.

use pg2sqlite::{
    errors::Error,
    prelude::{Pg2SqliteOptions, TranslationOptions},
};

mod helpers;

fn translate(sql: &str, opts: &Pg2SqliteOptions) -> Result<Vec<String>, Error> {
    helpers::translate_pg(sql, opts)
}

fn translate_with_geolite(sql: &str) -> Result<Vec<String>, Error> {
    translate(sql, &Pg2SqliteOptions::default().with_sqlitegis_enabled())
}

#[test]
fn gist_on_geometry_column_translates_to_create_spatial_index() {
    let sql = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
               CREATE INDEX features_geom_idx ON features USING gist (geom);";
    let stmts = translate_with_geolite(sql).expect("translate");
    let joined = stmts.join("\n");
    assert!(
        joined.contains("CreateSpatialIndex")
            && joined.contains("'features'")
            && joined.contains("'geom'"),
        "expected SELECT CreateSpatialIndex('features', 'geom') in output, got:\n{joined}"
    );
    assert!(
        !joined.to_ascii_uppercase().contains("CREATE INDEX FEATURES_GEOM_IDX"),
        "the CREATE INDEX statement should be replaced, got:\n{joined}"
    );
}

#[test]
fn gist_on_geography_column_translates_to_create_spatial_index() {
    let sql = "CREATE TABLE places (id INTEGER PRIMARY KEY, geog geography); \
               CREATE INDEX places_geog_idx ON places USING gist (geog);";
    let stmts = translate_with_geolite(sql).expect("translate");
    let joined = stmts.join("\n");
    assert!(
        joined.contains("CreateSpatialIndex")
            && joined.contains("'places'")
            && joined.contains("'geog'"),
        "got:\n{joined}"
    );
}

#[test]
fn gist_on_tsvector_still_routes_to_fts5_when_geolite_enabled() {
    // The existing FTS5 path must keep working when enable_geolite is on.
    let sql = "CREATE TABLE docs (id INTEGER PRIMARY KEY, body text); \
               CREATE INDEX docs_body_idx ON docs USING gist (to_tsvector('english', body));";
    let stmts = translate_with_geolite(sql).expect("translate");
    let joined = stmts.join("\n").to_ascii_uppercase();
    assert!(joined.contains("FTS5"), "expected FTS5 routing for to_tsvector, got:\n{joined}");
}

#[test]
fn gist_on_geometry_without_geolite_still_errors() {
    // Backward compat, without the option, the legacy error path fires.
    let sql = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
               CREATE INDEX features_geom_idx ON features USING gist (geom);";
    let result = translate(sql, &Pg2SqliteOptions::default());
    assert!(result.is_err(), "expected error without enable_geolite, got: {result:?}");
}

#[test]
fn gist_partial_index_on_geometry_errors() {
    // geolite's CreateSpatialIndex doesn't honor a WHERE predicate, so a
    // PG partial spatial index can't round-trip.
    let sql = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry, active boolean); \
               CREATE INDEX features_geom_idx ON features USING gist (geom) WHERE active;";
    let err = translate_with_geolite(sql).expect_err("partial spatial index must error");
    let msg = format!("{err:?}").to_ascii_lowercase();
    assert!(
        msg.contains("partial") || msg.contains("where") || msg.contains("predicate"),
        "error should explain partial-index limitation, got: {err:?}"
    );
}

#[test]
fn gist_on_mixed_columns_errors() {
    // GiST over (geom, name) where name is TEXT, translating only the geom
    // side would silently drop the user's intent. Error out.
    let sql = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry, name text); \
               CREATE INDEX features_mixed_idx ON features USING gist (geom, name);";
    let result = translate_with_geolite(sql);
    assert!(result.is_err(), "mixed-column GiST must error, got: {result:?}");
}

#[test]
fn gist_on_two_geometry_columns_emits_two_create_spatial_index_calls() {
    let sql = "CREATE TABLE features (id INTEGER PRIMARY KEY, a geometry, b geometry); \
               CREATE INDEX features_ab_idx ON features USING gist (a, b);";
    let stmts = translate_with_geolite(sql).expect("translate");
    let joined = stmts.join("\n");
    let count = joined.matches("CreateSpatialIndex").count();
    assert_eq!(count, 2, "expected 2 CreateSpatialIndex calls, got {count}:\n{joined}");
}
