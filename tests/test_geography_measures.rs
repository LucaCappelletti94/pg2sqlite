//! A measurement over a `geography` column is curved-earth, not planar.
//!
//! PostgreSQL measures `geography` on the WGS84 ellipsoid and answers metres.
//! The translator treated `geography` and `geometry` as one type, so every
//! measure forwarded to SQLiteGIS's planar implementation and answered
//! degrees: `ST_Distance` over a one-degree diagonal gave 1.41 where
//! PostgreSQL gives 156899.57, out by a factor of 110000 and in the wrong
//! unit.
//!
//! SQLiteGIS 0.1.5 carries an ellipsoid variant of all five measures, so every
//! one routes and every one is exact. The 0.1.0 the crate pinned before had
//! only a sphere length and no curved-earth area or perimeter at all, which is
//! why the dependency floor moved with this.
//!
//! Every PostgreSQL number below was read off PostGIS 3.5 on PostgreSQL 17.

#![cfg(feature = "sqlitegis")]

mod helpers;

use diesel::{QueryableByName, RunQueryDsl, sql_query, sql_types::Double};
use helpers::sqlitegis::sqlitegis_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

const SCHEMA: &str = "CREATE TABLE g (id INT PRIMARY KEY, geog geography, geom geometry);";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_sqlitegis_enabled()
}

fn translate(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{SCHEMA}\n{pg}"))
        .expect("parse")
        .translate_to_sql(&options())
        .expect("translate")
        .pop()
        .expect("a probe statement")
}

#[derive(QueryableByName)]
struct Measure {
    #[diesel(sql_type = Double)]
    val: f64,
}

/// Runs the emitted expression against SQLiteGIS, with the operands supplied
/// as SRID 4326 literals, which is what the curved-earth functions require.
fn evaluate(pg: &str, substitutions: &[(&str, &str)]) -> f64 {
    let mut probe = translate(pg);
    for (column, literal) in substitutions {
        probe = probe.replace(column, literal);
    }
    let sql =
        probe.replace(" FROM g", "").replacen("SELECT ", "SELECT CAST(", 1) + " AS REAL) AS val";
    let mut connection = sqlitegis_connection();
    sql_query(&sql)
        .load::<Measure>(&mut connection)
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{sql}"))
        .first()
        .expect("a row")
        .val
}

const POINT_A: &str = "ST_SetSRID(ST_Point(0, 0), 4326)";
const POINT_B: &str = "ST_SetSRID(ST_Point(1, 1), 4326)";
const LINE: &str = "ST_SetSRID(ST_GeomFromText('LINESTRING(0 0, 1 1)'), 4326)";
const SQUARE: &str = "ST_SetSRID(ST_GeomFromText('POLYGON((0 0,1 0,1 1,0 1,0 0))'), 4326)";

// ---------- what is routed ----------

#[test]
fn distance_over_geography_routes_to_the_ellipsoid() {
    assert_eq!(
        translate("SELECT ST_Distance(geog, geog) FROM g;"),
        "SELECT ST_DistanceSpheroid(geog, geog) FROM g"
    );
}

#[test]
fn the_radius_test_over_geography_routes_to_the_ellipsoid() {
    assert_eq!(
        translate("SELECT ST_DWithin(geog, geog, 100) FROM g;"),
        "SELECT ST_DWithinSpheroid(geog, geog, 100) FROM g"
    );
}

#[test]
fn length_over_geography_routes_to_the_ellipsoid() {
    assert_eq!(
        translate("SELECT ST_Length(geog) FROM g;"),
        "SELECT ST_LengthSpheroid(geog) FROM g"
    );
}

#[test]
fn area_and_perimeter_over_geography_route_to_the_ellipsoid() {
    assert_eq!(translate("SELECT ST_Area(geog) FROM g;"), "SELECT ST_AreaSpheroid(geog) FROM g");
    assert_eq!(
        translate("SELECT ST_Perimeter(geog) FROM g;"),
        "SELECT ST_PerimeterSpheroid(geog) FROM g"
    );
}

/// PostgreSQL answers 156899.56829134 metres. The planar emission answered
/// 1.4142135623730951 degrees.
#[test]
fn the_routed_distance_answers_metres_on_the_ellipsoid() {
    let metres = evaluate(
        "SELECT ST_Distance(geog, geog) FROM g;",
        &[("geog, geog", &format!("{POINT_A}, {POINT_B}"))],
    );
    assert!((metres - 156_899.568_291_34).abs() < 1e-6, "got {metres}");
}

/// PostgreSQL answers 156899.56829134029 metres for this line. The sphere
/// answers 157249.6, which is what an earlier SQLiteGIS could only offer, so
/// the tolerance here is tight enough to tell the two models apart.
#[test]
fn the_routed_length_answers_metres_on_the_ellipsoid() {
    let metres = evaluate("SELECT ST_Length(geog) FROM g;", &[("geog", LINE)]);
    assert!((metres - 156_899.568_291_340_29).abs() < 1.0, "got {metres}");
}

/// PostgreSQL answers 12308778361.469454 square metres and 443770.917248302
/// metres for this square. The planar emission answered 1 and 4, in degrees.
#[test]
fn the_routed_area_and_perimeter_answer_metres_on_the_ellipsoid() {
    let square =
        (metres_of("SELECT ST_Area(geog) FROM g;"), metres_of("SELECT ST_Perimeter(geog) FROM g;"));
    assert!((square.0 - 12_308_778_361.469_454).abs() < 1_000.0, "area {}", square.0);
    assert!((square.1 - 443_770.917_248_302).abs() < 1.0, "perimeter {}", square.1);
}

/// The square both of the above measure, as an SRID 4326 literal.
fn metres_of(pg: &str) -> f64 {
    evaluate(pg, &[("geog", SQUARE)])
}

// ---------- geometry is untouched ----------

/// A `geometry` column is planar in PostgreSQL too, so nothing about it
/// changes. This is what stops the routing from being applied to the whole
/// spatial family.
#[test]
fn every_measure_over_geometry_stays_planar() {
    for call in
        ["ST_Distance(geom, geom)", "ST_Length(geom)", "ST_Area(geom)", "ST_Perimeter(geom)"]
    {
        assert_eq!(translate(&format!("SELECT {call} FROM g;")), format!("SELECT {call} FROM g"));
    }
    assert_eq!(
        translate("SELECT ST_DWithin(geom, geom, 100) FROM g;"),
        "SELECT ST_DWithin(geom, geom, 100) FROM g"
    );
}

/// An operand whose type the schema does not settle keeps the planar
/// spelling, because guessing would silently change the unit.
#[test]
fn an_unresolved_operand_stays_planar() {
    assert_eq!(
        translate("SELECT ST_Distance(ST_Point(0, 0), ST_Point(1, 1)) FROM g;"),
        "SELECT ST_Distance(ST_Point(0, 0), ST_Point(1, 1)) FROM g"
    );
}

/// The non-metric predicates are shape questions rather than measurements, so
/// PostgreSQL answers them the same way for both types and they are left
/// alone.
#[test]
fn shape_predicates_over_geography_are_left_alone() {
    for call in ["ST_Intersects(geog, geog)", "ST_Covers(geog, geog)", "ST_Equals(geog, geog)"] {
        assert_eq!(translate(&format!("SELECT {call} FROM g;")), format!("SELECT {call} FROM g"));
    }
}

// ---------- ST_Buffer unit-mismatch refusal (R2-19) ----------

/// PostGIS `ST_Buffer` on a `geography` column computes in metres on the WGS84
/// ellipsoid. The SQLiteGIS passthrough is planar and reads the radius in
/// degrees, giving a result wrong by ~111000. The translator must refuse
/// rather than silently emit a wrong buffer.
///
/// Measured state before fix: both geometry and geography ST_Buffer passed
/// through without error. After the fix the geography case is refused.
#[test]
fn st_buffer_over_geography_is_refused() {
    let err = Pg2Sqlite::default()
        .sql(&format!("{SCHEMA}\nSELECT ST_Buffer(geog, 100) FROM g;"))
        .expect("parse")
        .translate_to_sql(&options())
        .expect_err("ST_Buffer on geography must be refused")
        .to_string();
    // The message must name the unit problem.
    assert!(
        err.to_lowercase().contains("metre")
            || err.to_lowercase().contains("meter")
            || err.to_lowercase().contains("degree")
            || err.to_lowercase().contains("unit"),
        "refusal must name the unit mismatch (metres vs degrees), got: {err}"
    );
}

/// `ST_Buffer` over a `geometry` column is planar in PostgreSQL too, so nothing
/// about it changes. This guards against the refusal being applied too broadly.
#[test]
fn st_buffer_over_geometry_passes_through() {
    assert_eq!(
        translate("SELECT ST_Buffer(geom, 100) FROM g;"),
        "SELECT ST_Buffer(geom, 100) FROM g"
    );
}
