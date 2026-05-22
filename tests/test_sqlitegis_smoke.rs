//! Step 1: confirm the SQLiteGIS test harness wires up correctly via Diesel.
//!
//! Only built when the `sqlitegis` cargo feature is enabled. Run with:
//!     cargo test --features sqlitegis --test test_sqlitegis_smoke

#![cfg(feature = "sqlitegis")]

mod helpers;

use diesel::{QueryableByName, RunQueryDsl, sql_query, sql_types::Text};
use helpers::sqlitegis::sqlitegis_connection;

#[derive(QueryableByName)]
struct TextRow {
    #[diesel(sql_type = Text)]
    val: String,
}

#[test]
fn sqlitegis_extension_executes_st_point_via_diesel() {
    let mut conn = sqlitegis_connection();
    let rows: Vec<TextRow> = sql_query("SELECT ST_AsText(ST_Point(0, 0)) AS val")
        .load(&mut conn)
        .expect("ST_AsText(ST_Point(0,0)) should execute against the SQLiteGIS-enabled connection");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].val, "POINT(0 0)");
}
