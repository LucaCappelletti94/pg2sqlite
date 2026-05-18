//! Step 2: PostGIS-equivalent types (`geometry`, `geography`) translate to
//! `BLOB` so that geolite's EWKB-encoded values can be stored and queried
//! at runtime, and the new `enable_geolite` option plumbs through
//! `Pg2SqliteOptions` for use by later steps.

use pg2sqlite::{
    pg2sqlite::Pg2Sqlite,
    prelude::{Pg2SqliteOptions, TranslationOptions},
};

#[test]
fn geometry_column_translates_to_blob() {
    let sqlite = Pg2Sqlite::default()
        .sql("CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry);")
        .expect("parse CREATE TABLE with geometry column")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("geometry should translate to BLOB");
    let joined = sqlite.join("\n");
    assert!(
        joined.to_ascii_uppercase().contains("BLOB"),
        "expected BLOB for geometry column, got: {joined}"
    );
}

#[test]
fn geography_column_translates_to_blob() {
    let sqlite = Pg2Sqlite::default()
        .sql("CREATE TABLE features (id INTEGER PRIMARY KEY, geog geography);")
        .expect("parse CREATE TABLE with geography column")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("geography should translate to BLOB");
    let joined = sqlite.join("\n");
    assert!(
        joined.to_ascii_uppercase().contains("BLOB"),
        "expected BLOB for geography column, got: {joined}"
    );
}

#[test]
fn enable_geolite_defaults_to_false() {
    let opts = Pg2SqliteOptions::default();
    assert!(!opts.is_geolite_enabled(), "enable_geolite should default to false");
}

#[test]
fn with_geolite_enabled_flips_flag() {
    let opts = Pg2SqliteOptions::default().with_geolite_enabled();
    assert!(opts.is_geolite_enabled());
}
