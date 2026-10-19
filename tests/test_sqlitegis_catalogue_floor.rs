//! Exercises native spatial helper arities and explicit owner isolation.

#![cfg(feature = "sqlitegis")]

mod helpers;

use diesel::{
    QueryableByName, RunQueryDsl, SqliteConnection,
    connection::SimpleConnection,
    sql_query,
    sql_types::{Double, Integer, Text},
};
use helpers::sqlitegis::sqlitegis_connection;
use pg2sqlite::{
    errors::{Error, RefusalCategory, TranslationDirection},
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};

#[derive(Debug, PartialEq, QueryableByName)]
struct Bounds {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Double)]
    xmin: f64,
    #[diesel(sql_type = Double)]
    xmax: f64,
    #[diesel(sql_type = Double)]
    ymin: f64,
    #[diesel(sql_type = Double)]
    ymax: f64,
}

#[derive(Debug, PartialEq, QueryableByName)]
struct Point {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Text)]
    point: String,
}

#[derive(QueryableByName)]
struct Status {
    #[diesel(sql_type = Integer)]
    status: i32,
}

#[derive(Debug, PartialEq, QueryableByName)]
struct Object {
    #[diesel(sql_type = Text)]
    name: String,
}

fn call(connection: &mut SqliteConnection, source: &str, reverse: bool) {
    let options = Pg2SqliteOptions::default().with_sqlitegis_enabled();
    let statements = if reverse {
        let translator = Pg2Sqlite::default();
        let schema = translator.build_schema().expect("empty source schema");
        translator.reverse_sql(source, &schema, &options).expect("reverse native helper")
    } else {
        Pg2Sqlite::default()
            .sql(source)
            .expect("source helper")
            .translate(&options)
            .expect("forward native helper")
            .into_vec()
    };
    // Emitted statements exercise the native helper ABI.
    for statement in statements {
        if matches!(&statement, sqlparser::ast::Statement::Query(_)) {
            let result = sql_query(statement.to_string())
                .get_result::<Status>(connection)
                .expect("native helper");
            assert_eq!(result.status, 1);
        } else {
            sql_query(statement.to_string()).execute(connection).expect("native setup");
        }
    }
}

fn bounds(connection: &mut SqliteConnection, owner: &str) -> Vec<Bounds> {
    // The owner selects the native index under test.
    sql_query(format!("SELECT id,xmin,xmax,ymin,ymax FROM {owner}.shapes_geom_rtree ORDER BY id"))
        .load(connection)
        .expect("native bounds")
}

fn points(connection: &mut SqliteConnection, owner: &str) -> Vec<Point> {
    // The owner selects the preserved base rows.
    sql_query(format!("SELECT id,ST_AsText(geom) AS point FROM {owner}.shapes ORDER BY id"))
        .load(connection)
        .expect("base points")
}

fn remaining_index(connection: &mut SqliteConnection, owner: &str) -> Vec<Object> {
    // The owner selects the catalogue affected by the drop.
    sql_query(format!("SELECT name FROM {owner}.sqlite_master WHERE name='shapes_geom_rtree'"))
        .load(connection)
        .expect("index catalogue")
}

fn expected_bounds(id: i32, x: f64, y: f64) -> Vec<Bounds> {
    vec![Bounds { id, xmin: x, xmax: x, ymin: y, ymax: y }]
}

#[test]
fn two_argument_helpers_preserve_ordinary_rows() {
    let mut connection = sqlitegis_connection();
    connection
        .batch_execute(
            "CREATE TABLE shapes(id INTEGER PRIMARY KEY,geom BLOB);\
         INSERT INTO shapes VALUES(11,ST_Point(2,3));",
        )
        .expect("ordinary fixture");
    call(&mut connection, "SELECT CreateSpatialIndex('shapes','geom') AS status", false);
    assert_eq!(bounds(&mut connection, "main"), expected_bounds(11, 2.0, 3.0));
    call(&mut connection, "SELECT DropSpatialIndex('shapes','geom') AS status", false);
    assert_eq!(remaining_index(&mut connection, "main"), Vec::<Object>::new());
    assert_eq!(points(&mut connection, "main"), vec![Point { id: 11, point: "POINT(2 3)".into() }]);
}

fn explicit_owners(reverse: bool) {
    let mut connection = sqlitegis_connection();
    connection
        .batch_execute(
            "CREATE TABLE main.shapes(id INTEGER PRIMARY KEY,geom BLOB);\
         CREATE TEMP TABLE shapes(id INTEGER PRIMARY KEY,geom BLOB);\
         INSERT INTO main.shapes VALUES(11,ST_Point(2,3));\
         INSERT INTO temp.shapes VALUES(22,ST_Point(7,8));",
        )
        .expect("colliding owner fixture");
    for owner in ["main", "temp"] {
        call(
            &mut connection,
            &format!("SELECT CreateSpatialIndex('{owner}','shapes','geom') AS status"),
            reverse,
        );
    }
    assert_eq!(bounds(&mut connection, "main"), expected_bounds(11, 2.0, 3.0));
    assert_eq!(bounds(&mut connection, "temp"), expected_bounds(22, 7.0, 8.0));
    call(&mut connection, "SELECT DropSpatialIndex('temp','shapes','geom') AS status", reverse);
    assert_eq!(remaining_index(&mut connection, "temp"), Vec::<Object>::new());
    assert_eq!(bounds(&mut connection, "main"), expected_bounds(11, 2.0, 3.0));
    call(&mut connection, "SELECT DropSpatialIndex('main','shapes','geom') AS status", reverse);
    assert_eq!(remaining_index(&mut connection, "main"), Vec::<Object>::new());
    assert_eq!(points(&mut connection, "main"), vec![Point { id: 11, point: "POINT(2 3)".into() }]);
    assert_eq!(points(&mut connection, "temp"), vec![Point { id: 22, point: "POINT(7 8)".into() }]);
}

#[test]
fn forward_three_argument_helpers_isolate_colliding_owners() {
    explicit_owners(false);
}

#[test]
fn reverse_three_argument_helpers_isolate_colliding_owners() {
    explicit_owners(true);
}

#[test]
fn unavailable_spatial_helper_calls_are_typed_refusals() {
    let translator = Pg2Sqlite::default();
    let schema = translator.build_schema().expect("empty source schema");
    for (source, options) in [
        (
            "SELECT CreateSpatialIndex('shapes')",
            Pg2SqliteOptions::default().with_sqlitegis_enabled(),
        ),
        (
            "SELECT DropSpatialIndex('main','shapes','geom',1)",
            Pg2SqliteOptions::default().with_sqlitegis_enabled(),
        ),
        ("SELECT CreateSpatialIndex('main','shapes','geom')", Pg2SqliteOptions::default()),
        ("SELECT DropSpatialIndex('main','shapes','geom')", Pg2SqliteOptions::default()),
    ] {
        for direction in
            [TranslationDirection::PostgreSqlToSqlite, TranslationDirection::SqliteToPostgreSql]
        {
            let result = match direction {
                TranslationDirection::PostgreSqlToSqlite => {
                    helpers::translate_pg(source, &options).map(|_| ())
                }
                TranslationDirection::SqliteToPostgreSql => {
                    translator.reverse_sql(source, &schema, &options).map(|_| ())
                }
            };
            let error = result.expect_err("unavailable helper");
            let Error::TranslationRefusal(refusal) = error else {
                panic!("typed refusal required");
            };
            assert_eq!(refusal.direction(), direction);
            assert_eq!(refusal.category(), RefusalCategory::UnrepresentableSemantics);
        }
    }
}
