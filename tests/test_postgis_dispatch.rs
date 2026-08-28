//! Step 3 (always-on): PostGIS function dispatch logic when
//! `Pg2SqliteOptions::sqlitegis_enabled` is on or off.
//!
//! These tests cover translation behavior only, no SQLite execution, so
//! they don't require the `sqlitegis` cargo feature. End-to-end execution
//! against the SQLiteGIS extension lives in `test_postgis_diesel.rs`.

use diesel::connection::SimpleConnection;
use pg2sqlite::prelude::Pg2SqliteOptions;

mod helpers;

#[test]
fn st_point_is_refused_when_sqlitegis_disabled() {
    // Without the extension SQLite has no `ST_Point`, so passing it through
    // emitted SQL that failed at run time with `no such function`. The
    // refusal names the option that would make it available.
    let error = helpers::translate_pg("SELECT ST_Point(0, 0) AS p;", &Pg2SqliteOptions::default())
        .expect_err("ST_Point has no SQLite form without SQLiteGIS")
        .to_string();
    assert!(error.contains("st_point"), "the error should name the function: {error}");
}

#[test]
fn st_point_passthrough_when_sqlitegis_enabled_and_arity_matches() {
    let opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();
    let translated = helpers::translate_pg("SELECT ST_Point(0, 0) AS p;", &opts)
        .expect("ST_Point/2 is in SQLiteGIS catalog and should pass through");
    let joined = translated.join("\n").to_ascii_uppercase();
    assert!(joined.contains("ST_POINT"), "got: {joined}");
    let mut conn = helpers::establish_connection();
    for stmt in &translated {
        // ST_Point is a SQLiteGIS UDF absent in the test process; accept that
        // specific error as proof that SQLite parsed the rest of the statement.
        match conn.batch_execute(stmt) {
            Ok(()) => {}
            Err(e) if e.to_string().contains("no such function: ST_Point") => {}
            Err(e) => panic!("SQLite rejected emitted syntax: {e}\n{stmt}"),
        }
    }
}

#[test]
fn st_point_wrong_arity_errors_when_sqlitegis_enabled() {
    let opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();
    let result = helpers::translate_pg("SELECT ST_Point(1) AS x;", &opts);
    let err = result.expect_err("ST_Point/1 is not in SQLiteGIS catalog, must error");
    let msg = format!("{err:?}");
    assert!(
        msg.to_ascii_lowercase().contains("st_point") && msg.contains('1'),
        "error should name the function and arity, got: {msg}"
    );
}

#[test]
fn st_transform_errors_when_sqlitegis_enabled() {
    // ST_Transform is a real PostGIS function but is NOT in SQLiteGIS's catalog
    // (no SRID reprojection yet). Hard error so users notice setup gaps.
    let opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();
    let result = helpers::translate_pg("SELECT ST_Transform(geom, 4326) AS x FROM t;", &opts);
    let err = result.expect_err("ST_Transform must error when SQLiteGIS is enabled");
    assert!(
        format!("{err:?}").to_ascii_lowercase().contains("st_transform"),
        "error should name the function, got: {err:?}"
    );
}
