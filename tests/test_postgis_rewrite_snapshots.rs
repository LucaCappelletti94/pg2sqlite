//! Verbatim-text snapshots of the translated SQL for the SELECT, UPDATE,
//! and DELETE spatial predicate rewrites. Substring assertions in
//! `test_postgis_predicate_rewrite.rs` confirm the rewrite fires; these
//! snapshots additionally lock down the exact emitted text so any
//! unintentional drift in projection ordering, bbox bounds, parenthesization,
//! or the rtree subquery shape surfaces as a snapshot diff rather than a
//! silent semantic change.

use pg2sqlite::prelude::{Pg2SqliteOptions, TranslationOptions};

mod helpers;

fn translate_to_text(pg: &str) -> String {
    let opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();
    helpers::translate_pg(pg, &opts).expect("translate").join("\n")
}

#[test]
fn snapshot_select_rewrite_via_rtree() {
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              SELECT id FROM features \
               WHERE ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10));";
    insta::assert_snapshot!("select_rewrite_via_rtree", translate_to_text(pg));
}

#[test]
fn snapshot_update_rewrite_via_rtree() {
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry, name text); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              UPDATE features SET name = 'hit' \
               WHERE ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10));";
    insta::assert_snapshot!("update_rewrite_via_rtree", translate_to_text(pg));
}

#[test]
fn snapshot_delete_rewrite_via_rtree() {
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              DELETE FROM features WHERE ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10));";
    insta::assert_snapshot!("delete_rewrite_via_rtree", translate_to_text(pg));
}

#[test]
fn snapshot_select_with_extra_conjuncts() {
    // Mixed WHERE: spatial conjunct + non-spatial conjunct. The rewrite must
    // preserve the non-spatial filter as a final-pass condition alongside
    // the original ST_Intersects.
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry, kind text); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              SELECT id FROM features \
               WHERE kind = 'park' \
                 AND ST_Intersects(geom, ST_MakeEnvelope(0, 0, 10, 10));";
    insta::assert_snapshot!("select_with_extra_conjuncts", translate_to_text(pg));
}

#[test]
fn snapshot_select_with_qualified_column_ref() {
    // First arg of ST_Intersects is `features.geom` rather than bare `geom`.
    // The qualifier resolution should still match the catalog entry and the
    // rewritten rtree subquery should look identical to the bare-ref case.
    let pg = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
              CREATE INDEX features_geom_idx ON features USING gist (geom); \
              SELECT id FROM features \
               WHERE ST_Intersects(features.geom, ST_MakeEnvelope(0, 0, 10, 10));";
    insta::assert_snapshot!("select_with_qualified_column_ref", translate_to_text(pg));
}
