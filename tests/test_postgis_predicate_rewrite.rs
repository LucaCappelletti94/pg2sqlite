//! Step 6 (always-on): when a query's WHERE clause contains a
//! bbox-narrowable PostGIS predicate over a column whose table was given a
//! `USING gist (...)` index in the same translation unit, pg2sqlite
//! rewrites the SELECT to JOIN against the rtree shadow so SQLite's planner
//! actually uses the index. Anything outside the conservative single-table,
//! flat-AND, simple-column-ref shape falls through to passthrough.

use pg2sqlite::{
    errors::Error,
    prelude::{Pg2SqliteOptions, TranslationOptions},
};

mod helpers;

fn translate_with_sqlitegis(sql: &str) -> Result<Vec<String>, Error> {
    helpers::translate_pg(sql, &Pg2SqliteOptions::default().with_sqlitegis_enabled())
}

fn user_select(stmts: &[String]) -> String {
    helpers::user_statement_of(stmts, "SELECT").clone()
}

fn user_dml(stmts: &[String], kind: &str) -> String {
    helpers::user_statement_of(stmts, kind).clone()
}

#[test]
fn st_intersects_on_indexed_column_rewrites_with_rtree_join() {
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              SELECT id FROM features \
               WHERE ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10));";
    let stmts = translate_with_sqlitegis(pg).expect("translate");
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
        let stmts = translate_with_sqlitegis(&pg).expect("translate");
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
    let stmts = translate_with_sqlitegis(pg).expect("translate");
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
    let stmts = translate_with_sqlitegis(pg).expect("translate");
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
    let stmts = translate_with_sqlitegis(pg).expect("translate");
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
    let stmts = translate_with_sqlitegis(pg).expect("translate");
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
    let stmts = translate_with_sqlitegis(pg).expect("translate");
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
    let stmts = translate_with_sqlitegis(pg).expect("translate");
    let select = user_select(&stmts);
    assert!(
        select.contains("features_geom_rtree"),
        "DISTINCT ON SELECT must still reach the spatial rewrite, got:\n{select}"
    );
}

#[test]
fn update_indexed_column_rewrites_via_rtree() {
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry, name text); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              UPDATE features SET name = 'hit' \
               WHERE ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10));";
    let stmts = translate_with_sqlitegis(pg).expect("translate");
    let update = user_dml(&stmts, "UPDATE");
    assert!(
        update.contains("features_geom_rtree"),
        "expected rtree filter in rewritten UPDATE, got:\n{update}"
    );
    assert!(
        update.to_ascii_uppercase().contains("ST_INTERSECTS"),
        "original predicate must remain as final-pass filter, got:\n{update}"
    );
}

#[test]
fn delete_indexed_column_rewrites_via_rtree() {
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              DELETE FROM features WHERE ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10));";
    let stmts = translate_with_sqlitegis(pg).expect("translate");
    let delete = user_dml(&stmts, "DELETE");
    assert!(
        delete.contains("features_geom_rtree"),
        "expected rtree filter in rewritten DELETE, got:\n{delete}"
    );
    assert!(
        delete.to_ascii_uppercase().contains("ST_INTERSECTS"),
        "original predicate must remain as final-pass filter, got:\n{delete}"
    );
}

#[test]
fn update_without_where_does_not_rewrite() {
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry, name text); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              UPDATE features SET name = 'all';";
    let stmts = translate_with_sqlitegis(pg).expect("translate");
    let update = user_dml(&stmts, "UPDATE");
    assert!(
        !update.contains("_rtree"),
        "UPDATE without WHERE has no spatial filter to rewrite, got:\n{update}"
    );
}

#[test]
fn delete_without_where_does_not_rewrite() {
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              DELETE FROM features;";
    let stmts = translate_with_sqlitegis(pg).expect("translate");
    let delete = user_dml(&stmts, "DELETE");
    assert!(
        !delete.contains("_rtree"),
        "DELETE without WHERE has no spatial filter to rewrite, got:\n{delete}"
    );
}

#[test]
fn update_with_or_in_where_does_not_rewrite() {
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry, name text, kind text); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              UPDATE features SET name = 'hit' \
               WHERE ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10)) OR kind = 'pinned';";
    let stmts = translate_with_sqlitegis(pg).expect("translate");
    let update = user_dml(&stmts, "UPDATE");
    assert!(
        !update.contains("features_geom_rtree"),
        "top-level OR must disable UPDATE rewrite, got:\n{update}"
    );
}

#[test]
fn delete_with_or_in_where_does_not_rewrite() {
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry, kind text); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              DELETE FROM features \
               WHERE ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10)) OR kind = 'pinned';";
    let stmts = translate_with_sqlitegis(pg).expect("translate");
    let delete = user_dml(&stmts, "DELETE");
    assert!(
        !delete.contains("features_geom_rtree"),
        "top-level OR must disable DELETE rewrite, got:\n{delete}"
    );
}

#[test]
fn update_on_unindexed_column_does_not_rewrite() {
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, g geometry, name text); \
              UPDATE features SET name = 'hit' \
               WHERE ST_Intersects(g, ST_MakeEnvelope(0, 0, 10, 10));";
    let stmts = translate_with_sqlitegis(pg).expect("translate");
    let update = user_dml(&stmts, "UPDATE");
    assert!(
        !update.to_ascii_lowercase().contains("_rtree"),
        "no spatial index means no rewrite, got:\n{update}"
    );
}

#[test]
fn delete_on_unindexed_column_does_not_rewrite() {
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, g geometry); \
              DELETE FROM features WHERE ST_Intersects(g, ST_MakeEnvelope(0, 0, 10, 10));";
    let stmts = translate_with_sqlitegis(pg).expect("translate");
    let delete = user_dml(&stmts, "DELETE");
    assert!(
        !delete.to_ascii_lowercase().contains("_rtree"),
        "no spatial index means no rewrite, got:\n{delete}"
    );
}

#[test]
fn delete_with_using_does_not_rewrite() {
    // DELETE ... USING translates to a DELETE whose WHERE is wrapped in
    // EXISTS(subquery). Multi-source statements are out of scope and the
    // EXISTS wrap means the WHERE is no longer a flat AND, so the rewrite
    // naturally bails.
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              CREATE TABLE tags (feature_id INTEGER); \
              DELETE FROM features USING tags \
               WHERE features.id = tags.feature_id \
                 AND ST_Intersects(features.geom, ST_MakeEnvelope(0, 0, 10, 10));";
    let stmts = translate_with_sqlitegis(pg).expect("translate");
    let delete = user_dml(&stmts, "DELETE");
    assert!(
        !delete.contains("features_geom_rtree"),
        "multi-source DELETE ... USING must not be rewritten, got:\n{delete}"
    );
}

#[test]
fn rewrite_disabled_without_sqlitegis_flag() {
    // Same DDL + query, but without sqlitegis_enabled. The rewrite (and the
    // CreateSpatialIndex DDL) must not fire; the SELECT translates verbatim.
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              SELECT id FROM features \
               WHERE ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10));";
    // Without sqlitegis_enabled the CREATE INDEX USING gist errors out today,
    // so this assertion just checks the option-gating is consistent; if Step 4
    // ever relaxes, the test should also assert no rtree JOIN appears.
    let result = helpers::translate_pg(pg, &Pg2SqliteOptions::default());
    assert!(result.is_err(), "without sqlitegis_enabled the GiST DDL still errors");
}
