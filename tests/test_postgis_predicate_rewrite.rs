//! Step 6 (always-on): when a query's WHERE clause contains a
//! bbox-narrowable PostGIS predicate over a column whose table was given a
//! `USING gist (...)` index in the same translation unit, pg2sqlite
//! rewrites the SELECT to JOIN against the rtree shadow so SQLite's planner
//! actually uses the index. Anything outside the conservative single-table,
//! flat-AND, simple-column-ref shape falls through to passthrough.

use pg2sqlite::{
    errors::Error,
    pg2sqlite::Pg2Sqlite,
    prelude::{Pg2SqliteOptions, TranslationOptions},
};

fn translate_with_geolite(sql: &str) -> Result<Vec<String>, Error> {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default().with_geolite_enabled())
}

/// Returns the translated user SELECT (the one that is not a
/// `SELECT CreateSpatialIndex(...)` call emitted by index translation).
fn user_select(stmts: &[String]) -> String {
    stmts
        .iter()
        .find(|s| {
            let upper = s.to_ascii_uppercase();
            upper.trim_start().starts_with("SELECT") && !s.contains("CreateSpatialIndex")
        })
        .unwrap_or_else(|| panic!("no user SELECT in:\n{}", stmts.join("\n")))
        .clone()
}

#[test]
fn st_intersects_on_indexed_column_rewrites_with_rtree_join() {
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              SELECT id FROM features \
               WHERE ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10));";
    let stmts = translate_with_geolite(pg).expect("translate");
    let select = user_select(&stmts);
    assert!(
        select.contains("features_geom_rtree"),
        "expected rtree JOIN in the rewritten SELECT, got:\n{select}"
    );
    assert!(
        select.contains("xmin") && select.contains("xmax"),
        "expected bbox conditions on the rtree shadow, got:\n{select}"
    );
    // The original predicate must be preserved as the final-pass filter.
    assert!(
        select.to_ascii_uppercase().contains("ST_INTERSECTS"),
        "the ST_Intersects predicate must remain to filter the rtree candidates, got:\n{select}"
    );
}

#[test]
fn each_bbox_narrowable_predicate_rewrites() {
    for predicate in [
        "ST_Intersects",
        "ST_Contains",
        "ST_Within",
        "ST_Covers",
        "ST_CoveredBy",
        "ST_Equals",
        "ST_Touches",
        "ST_Crosses",
        "ST_Overlaps",
    ] {
        let pg = format!(
            "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
             CREATE INDEX features_geom_idx ON features USING gist (geom); \
             SELECT id FROM features \
              WHERE {predicate}(geom, ST_MakeEnvelope(0, 0, 10, 10));"
        );
        let stmts = translate_with_geolite(&pg).expect("translate");
        let select = user_select(&stmts);
        assert!(
            select.contains("features_geom_rtree"),
            "{predicate} should be bbox-narrowable; expected rtree JOIN, got:\n{select}"
        );
    }
}

#[test]
fn st_disjoint_does_not_rewrite() {
    // ST_Disjoint is the inverse of ST_Intersects; a positive answer does
    // NOT imply bbox overlap, so an rtree pre-filter would silently drop
    // correct results.
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              SELECT id FROM features \
               WHERE ST_Disjoint(geom, ST_MakeEnvelope(0, 0, 10, 10));";
    let stmts = translate_with_geolite(pg).expect("translate");
    let select = user_select(&stmts);
    assert!(
        !select.contains("features_geom_rtree"),
        "ST_Disjoint must not be rewritten via rtree (would drop correct rows), got:\n{select}"
    );
}

#[test]
fn intersects_on_unindexed_column_does_not_rewrite() {
    // No GiST/spatial index over `g`; the rewrite has nothing to join to.
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, g geometry); \
              SELECT id FROM features \
               WHERE ST_Intersects(g, ST_MakeEnvelope(0, 0, 10, 10));";
    let stmts = translate_with_geolite(pg).expect("translate");
    let select = user_select(&stmts);
    assert!(
        !select.to_ascii_lowercase().contains("_rtree"),
        "no spatial index means no rtree JOIN, got:\n{select}"
    );
}

#[test]
fn or_in_where_disables_rewrite() {
    // A top-level OR means the spatial predicate is not a required filter,
    // so adding the rtree JOIN would drop rows the other disjunct selects.
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry, kind text); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              SELECT id FROM features \
               WHERE ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10)) OR kind = 'pinned';";
    let stmts = translate_with_geolite(pg).expect("translate");
    let select = user_select(&stmts);
    assert!(
        !select.contains("features_geom_rtree"),
        "top-level OR must disable the rewrite, got:\n{select}"
    );
}

#[test]
fn join_in_from_disables_rewrite() {
    // Multi-table FROM list. v1 conservatively skips these.
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              CREATE TABLE tags (feature_id INTEGER, label text); \
              SELECT f.id FROM features f JOIN tags t ON t.feature_id = f.id \
               WHERE ST_Intersects(f.geom, ST_MakeEnvelope(0, 0, 10, 10));";
    let stmts = translate_with_geolite(pg).expect("translate");
    let select = user_select(&stmts);
    assert!(
        !select.contains("features_geom_rtree"),
        "multi-table FROM must disable the rewrite in v1, got:\n{select}"
    );
}

#[test]
fn non_simple_first_arg_disables_rewrite() {
    // First arg is a function call, not a bare column reference. The
    // bbox-via-rtree pre-filter is only valid when one side of the predicate
    // is the indexed column itself; an expression like ST_Buffer(geom, ...)
    // has its own bbox that the rtree doesn't track.
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              SELECT id FROM features \
               WHERE ST_Intersects(ST_Buffer(geom, 1.0), ST_MakeEnvelope(0, 0, 10, 10));";
    let stmts = translate_with_geolite(pg).expect("translate");
    let select = user_select(&stmts);
    assert!(
        !select.contains("features_geom_rtree"),
        "non-trivial first arg must disable the rewrite, got:\n{select}"
    );
}

#[test]
fn distinct_on_select_also_reaches_the_rewrite() {
    // `SELECT DISTINCT ON (...)` triggers `try_translate_distinct_on_query`
    // in query.rs which calls `<Select as Translator>::translate` directly,
    // bypassing the `SetExpr::Select` dispatch in `translate_set_expr_shared`.
    // The rewrite hook lives inside `translate_select_shared` so both entry
    // points converge on it; this regression test pins that behavior.
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry, kind text); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              SELECT DISTINCT ON (kind) id, kind FROM features \
               WHERE ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10));";
    let stmts = translate_with_geolite(pg).expect("translate");
    let select = user_select(&stmts);
    assert!(
        select.contains("features_geom_rtree"),
        "DISTINCT ON SELECT must still reach the spatial rewrite, got:\n{select}"
    );
}

#[test]
fn rewrite_disabled_without_geolite_flag() {
    // Same DDL + query, but without enable_geolite. The rewrite (and the
    // CreateSpatialIndex DDL) must not fire; the SELECT translates verbatim.
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              SELECT id FROM features \
               WHERE ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10));";
    // Without enable_geolite the CREATE INDEX USING gist errors out today,
    // so this assertion just checks the option-gating is consistent; if Step 4
    // ever relaxes, the test should also assert no rtree JOIN appears.
    let result =
        Pg2Sqlite::default().sql(pg).expect("parse").translate_to_sql(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "without enable_geolite the GiST DDL still errors");
}
