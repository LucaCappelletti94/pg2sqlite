//! Step 2: PostGIS-equivalent types (`geometry`, `geography`) translate to
//! `BLOB` so that SQLiteGIS's EWKB-encoded values can be stored and queried
//! at runtime, and the new `sqlitegis_enabled` option plumbs through
//! `Pg2SqliteOptions` for use by later steps.

use pg2sqlite::{pg2sqlite::Pg2Sqlite, prelude::Pg2SqliteOptions};

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
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for stmt in &sqlite {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("emitted DDL must run in SQLite: {e}\n{stmt}"));
    }
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
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for stmt in &sqlite {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("emitted DDL must run in SQLite: {e}\n{stmt}"));
    }
}

#[test]
fn sqlitegis_enabled_defaults_to_false() {
    let opts = Pg2SqliteOptions::default();
    assert!(!opts.is_sqlitegis_enabled(), "sqlitegis_enabled should default to false");
}

#[test]
fn with_sqlitegis_enabled_flips_flag() {
    let opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();
    assert!(opts.is_sqlitegis_enabled());
}
