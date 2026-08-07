//! Reference-closed translation: a foreign key naming a table or column the
//! document does not declare, or declares only later, fails the schema build
//! the way the script would fail PostgreSQL's sequential DDL apply. There is
//! no opt-out, so a fragment becomes translatable by declaring its targets
//! earlier in the same document.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

const DANGLING_TABLE: &str =
    "CREATE TABLE child (id INT PRIMARY KEY, parent_id INT REFERENCES missing_table(id));";

const DANGLING_COLUMN: &str = "\
CREATE TABLE parent (id INT PRIMARY KEY);
CREATE TABLE child (id INT PRIMARY KEY, parent_id INT REFERENCES parent(missing_col));";

const FORWARD_REFERENCE: &str = "\
CREATE TABLE child (id INT PRIMARY KEY, parent_id INT REFERENCES parent(id));
CREATE TABLE parent (id INT PRIMARY KEY);";

#[test]
fn dangling_table_fails_translate_to_sql() {
    let err = Pg2Sqlite::default()
        .sql(DANGLING_TABLE)
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("dangling FK table target must fail translation");
    let msg = err.to_string();
    assert!(msg.contains("missing_table"), "error should name the missing target: {msg}");
    assert!(msg.contains("child"), "error should name the owning table: {msg}");
}

#[test]
fn dangling_table_fails_translate_with_report() {
    let err = Pg2Sqlite::default()
        .sql(DANGLING_TABLE)
        .unwrap()
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect_err("dangling FK table target must fail report translation");
    assert!(err.to_string().contains("missing_table"));
}

#[test]
fn dangling_table_fails_translation_manifest() {
    let err = Pg2Sqlite::default()
        .sql(DANGLING_TABLE)
        .unwrap()
        .translation_manifest(&Pg2SqliteOptions::default())
        .expect_err("dangling FK table target must fail manifest");
    let msg = err.to_string();
    assert!(msg.contains("missing_table"), "manifest error should name the missing target: {msg}");
    assert!(msg.contains("child"), "manifest error should name the owning table: {msg}");
}

#[test]
fn dangling_column_fails_translate_to_sql() {
    let err = Pg2Sqlite::default()
        .sql(DANGLING_COLUMN)
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("dangling FK column target must fail translation");
    let msg = err.to_string();
    assert!(msg.contains("missing_col"), "error should name the missing column: {msg}");
    assert!(msg.contains("child"), "error should name the owning table: {msg}");
}

#[test]
fn a_forward_reference_is_refused_as_postgres_applies_ddl_in_order() {
    let err = Pg2Sqlite::default()
        .sql(FORWARD_REFERENCE)
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("PostgreSQL applies DDL in order, so a forward reference must fail");
    let msg = err.to_string();
    assert!(msg.contains("parent"), "error should name the missing target: {msg}");
    assert!(msg.contains("child"), "error should name the owning table: {msg}");
}

#[test]
fn closing_the_document_translates_the_former_fragment() {
    let sql = Pg2Sqlite::default()
        .sql("CREATE TABLE missing_table (id INT PRIMARY KEY);")
        .unwrap()
        .sql(DANGLING_TABLE)
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("declaring the target in the same document must translate");
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    for s in &sql {
        diesel::sql_query(s.as_str()).execute(&mut conn).unwrap();
    }
}

#[test]
fn closing_the_document_builds_the_manifest() {
    let manifest = Pg2Sqlite::default()
        .sql("CREATE TABLE missing_table (id INT PRIMARY KEY);")
        .unwrap()
        .sql(DANGLING_TABLE)
        .unwrap()
        .translation_manifest(&Pg2SqliteOptions::default())
        .expect("declaring the target in the same document must build the manifest");
    assert!(manifest.iter().any(|entry| entry.logical == "child"));
}
