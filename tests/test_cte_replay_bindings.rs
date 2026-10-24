//! Connection replay keeps a lexical common table expression distinct from
//! the physical relation of the same name it shadows.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::*;

mod schema {
    diesel::table! {
        result (id) { id -> Integer, }
    }
    diesel::table! {
        mixed_out (a, b) { a -> Integer, b -> Integer, }
    }
    diesel::table! {
        shadow_out (outer_id, inner_id) { outer_id -> Integer, inner_id -> Integer, }
    }
}

fn script(sql: &str) -> Script<String> {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
}

fn open(files: &tempfile::TempDir) -> SqliteConnection {
    SqliteConnection::establish(files.path().join("main.db").to_str().expect("path")).expect("open")
}

fn run<'a>(connection: &mut SqliteConnection, statements: impl Iterator<Item = &'a String>) {
    for statement in statements {
        connection.batch_execute(statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
}

/// Proves the freshly created store answers 42 for the initial view row.
fn expect_initial_row(files: &tempfile::TempDir, script: &Script<String>) {
    let mut creating = open(files);
    run(&mut creating, script.iter());
    let row = schema::result::table
        .select(schema::result::id)
        .first::<i32>(&mut creating)
        .expect("initial view row");
    assert_eq!(row, 42);
}

/// Proves the reopened store still answers 42 after the connection program.
fn expect_replayed_row(files: &tempfile::TempDir, script: &Script<String>) {
    let mut reopened = open(files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = schema::result::table
        .select(schema::result::id)
        .first::<i32>(&mut reopened)
        .expect("replayed view row");
    assert_eq!(row, 42);
}

#[test]
fn replayed_cte_wildcard_keeps_its_lexical_value() {
    let script = script(
        "CREATE TABLE papers(id INT PRIMARY KEY);
         INSERT INTO papers VALUES(1);
         CREATE TEMP VIEW result AS
             WITH papers AS (SELECT 42 AS id, 99 AS renamed)
             SELECT id FROM (SELECT * FROM papers) AS nested;
         ALTER TABLE papers RENAME COLUMN id TO renamed;",
    );
    let files = tempfile::TempDir::new().expect("files");
    expect_initial_row(&files, &script);
    expect_replayed_row(&files, &script);
}

#[test]
fn replayed_qualified_wildcard_stays_lexical() {
    let script = script(
        "CREATE TABLE papers(id INT PRIMARY KEY, n INT);
         INSERT INTO papers VALUES(1, 10);
         CREATE TEMP VIEW result AS WITH papers AS (SELECT 42 AS id) SELECT papers.* FROM papers;
         ALTER TABLE papers RENAME TO archive;",
    );
    let files = tempfile::TempDir::new().expect("files");
    expect_initial_row(&files, &script);
    expect_replayed_row(&files, &script);
}

#[test]
fn replayed_connect_ignores_an_unused_physical_column_drop() {
    let script = script(
        "CREATE TABLE papers(id INT PRIMARY KEY, n INT);
         INSERT INTO papers VALUES(1, 10);
         CREATE TEMP VIEW result AS WITH papers AS (SELECT 42 AS id) SELECT id FROM papers;
         ALTER TABLE papers DROP COLUMN n;",
    );
    let files = tempfile::TempDir::new().expect("files");
    expect_initial_row(&files, &script);
    expect_replayed_row(&files, &script);
}

#[test]
fn replayed_mixed_frame_keeps_the_cte_slot() {
    let script = script(
        "CREATE TABLE papers(id INT PRIMARY KEY, n INT);
         INSERT INTO papers VALUES(1, 10);
         CREATE TEMP VIEW mixed_out AS
             WITH cte AS (SELECT 42 AS a, 55 AS b)
             SELECT a, b FROM (SELECT * FROM cte JOIN papers ON true) AS mixed;
         ALTER TABLE papers RENAME COLUMN n TO total;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
        let row = schema::mixed_out::table
            .select((schema::mixed_out::a, schema::mixed_out::b))
            .first::<(i32, i32)>(&mut creating)
            .expect("initial view row");
        assert_eq!(row, (42, 55));
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = schema::mixed_out::table
        .select((schema::mixed_out::a, schema::mixed_out::b))
        .first::<(i32, i32)>(&mut reopened)
        .expect("replayed view row");
    assert_eq!(row, (42, 55));
}

#[test]
fn replayed_inner_cte_shadows_the_outer_physical_table() {
    let script = script(
        "CREATE TABLE papers(id INT PRIMARY KEY);
         INSERT INTO papers VALUES(1);
         CREATE TEMP VIEW shadow_out AS
             SELECT p.id AS outer_id, n.id AS inner_id
             FROM papers AS p
             CROSS JOIN (WITH papers AS (SELECT 42 AS id) SELECT * FROM papers) AS n;
         ALTER TABLE papers RENAME COLUMN id TO renamed;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
        let row = schema::shadow_out::table
            .select((schema::shadow_out::outer_id, schema::shadow_out::inner_id))
            .first::<(i32, i32)>(&mut creating)
            .expect("initial view row");
        assert_eq!(row, (1, 42));
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = schema::shadow_out::table
        .select((schema::shadow_out::outer_id, schema::shadow_out::inner_id))
        .first::<(i32, i32)>(&mut reopened)
        .expect("replayed view row");
    assert_eq!(row, (1, 42));
}
