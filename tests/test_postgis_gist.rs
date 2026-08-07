//! Step 4 (always-on): GiST indexes on geometry/geography columns translate
//! into `SELECT CreateSpatialIndex('tbl', 'col')` calls that SQLiteGIS
//! executes at runtime to set up an rtree shadow table.
//!
//! These tests cover the dispatch behavior only, no SQLite execution.
//! The end-to-end Diesel variant lives in `test_postgis_gist_diesel.rs`.

use diesel::connection::SimpleConnection;
use pg2sqlite::{
    errors::Error,
    prelude::{Pg2SqliteOptions, TranslationOptions},
};

mod helpers;

fn translate(sql: &str, opts: &Pg2SqliteOptions) -> Result<Vec<String>, Error> {
    helpers::translate_pg(sql, opts)
}

fn translate_with_sqlitegis(sql: &str) -> Result<Vec<String>, Error> {
    translate(sql, &Pg2SqliteOptions::default().with_sqlitegis_enabled())
}

/// Executes each translated statement against an in-memory SQLite connection.
/// DDL is run directly. SELECT statements (e.g. `SELECT
/// CreateSpatialIndex(...)`) are also run; the SQLiteGIS UDF is absent in the
/// test process, so a `no such function: CreateSpatialIndex` error is accepted
/// as proof that SQLite parsed and accepted the rest of the statement.
fn run_sqlitegis_stmts(stmts: &[String]) {
    let mut conn = helpers::establish_connection();
    for s in stmts {
        match conn.batch_execute(s) {
            Ok(()) => {}
            Err(e) if e.to_string().contains("no such function: CreateSpatialIndex") => {}
            Err(e) => panic!("SQLite rejected emitted SQL: {e}\n{s}"),
        }
    }
}

#[test]
fn gist_on_geometry_column_translates_to_create_spatial_index() {
    let sql = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
               CREATE INDEX features_geom_idx ON features USING gist (geom);";
    let stmts = translate_with_sqlitegis(sql).expect("translate");
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
    // CreateSpatialIndex requires SQLiteGIS; DDL statements execute fine;
    // SELECT CreateSpatialIndex(...) is prepared to prove SQLite accepts the
    // syntax.
    run_sqlitegis_stmts(&stmts);
}

#[test]
fn gist_on_geography_column_translates_to_create_spatial_index() {
    let sql = "CREATE TABLE places (id INTEGER PRIMARY KEY, geog geography); \
               CREATE INDEX places_geog_idx ON places USING gist (geog);";
    let stmts = translate_with_sqlitegis(sql).expect("translate");
    let joined = stmts.join("\n");
    assert!(
        joined.contains("CreateSpatialIndex")
            && joined.contains("'places'")
            && joined.contains("'geog'"),
        "got:\n{joined}"
    );
    // CreateSpatialIndex requires SQLiteGIS; DDL statements execute fine;
    // SELECT CreateSpatialIndex(...) is prepared to prove SQLite accepts the
    // syntax.
    run_sqlitegis_stmts(&stmts);
}

#[test]
fn gist_on_tsvector_still_routes_to_fts5_when_sqlitegis_enabled() {
    // The existing FTS5 path must keep working when sqlitegis_enabled is on.
    let sql = "CREATE TABLE docs (id INTEGER PRIMARY KEY, body text); \
               CREATE INDEX docs_body_idx ON docs USING gist (to_tsvector('english', body));";
    let stmts = translate_with_sqlitegis(sql).expect("translate");
    let joined = stmts.join("\n").to_ascii_uppercase();
    assert!(joined.contains("FTS5"), "expected FTS5 routing for to_tsvector, got:\n{joined}");
    // The FTS5 path is standard SQLite; execute_batch proves SQLite accepts the
    // output.
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("SQLite rejected emitted SQL: {e}\n{s}"));
    }
}

#[test]
fn gist_on_geometry_without_sqlitegis_still_errors() {
    // Backward compat, without the option, the legacy error path fires.
    let sql = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
               CREATE INDEX features_geom_idx ON features USING gist (geom);";
    let result = translate(sql, &Pg2SqliteOptions::default());
    assert!(result.is_err(), "expected error without sqlitegis_enabled, got: {result:?}");
}

#[test]
fn gist_partial_index_on_geometry_errors() {
    // SQLiteGIS's CreateSpatialIndex doesn't honor a WHERE predicate, so a
    // PG partial spatial index can't round-trip.
    let sql = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry, active boolean); \
               CREATE INDEX features_geom_idx ON features USING gist (geom) WHERE active;";
    let err = translate_with_sqlitegis(sql).expect_err("partial spatial index must error");
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
    let result = translate_with_sqlitegis(sql);
    assert!(result.is_err(), "mixed-column GiST must error, got: {result:?}");
}

#[test]
fn gist_on_two_geometry_columns_emits_two_create_spatial_index_calls() {
    let sql = "CREATE TABLE features (id INTEGER PRIMARY KEY, a geometry, b geometry); \
               CREATE INDEX features_ab_idx ON features USING gist (a, b);";
    let stmts = translate_with_sqlitegis(sql).expect("translate");
    let joined = stmts.join("\n");
    let count = joined.matches("CreateSpatialIndex").count();
    assert_eq!(count, 2, "expected 2 CreateSpatialIndex calls, got {count}:\n{joined}");
    // CreateSpatialIndex requires SQLiteGIS; DDL statements execute fine;
    // SELECT CreateSpatialIndex(...) is prepared to prove SQLite accepts the
    // syntax.
    run_sqlitegis_stmts(&stmts);
}
