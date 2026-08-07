//! Reference-closed translation: `ParserDB::validate_foreign_key_targets`
//! wired into the emission boundary, with the
//! `with_dangling_foreign_keys_allowed` opt-out.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, TranslationOptions};

const DANGLING_TABLE: &str =
    "CREATE TABLE child (id INT PRIMARY KEY, parent_id INT REFERENCES missing_table(id));";

const DANGLING_COLUMN: &str = "\
CREATE TABLE parent (id INT PRIMARY KEY);
CREATE TABLE child (id INT PRIMARY KEY, parent_id INT REFERENCES parent(missing_col));";

const FORWARD_REFERENCE: &str = "\
CREATE TABLE child (id INT PRIMARY KEY, parent_id INT REFERENCES parent(id));
CREATE TABLE parent (id INT PRIMARY KEY);";

#[test]
fn dangling_foreign_keys_disallowed_by_default() {
    assert!(!Pg2SqliteOptions::default().is_dangling_foreign_keys_allowed());
}

#[test]
fn with_dangling_foreign_keys_allowed_sets_flag() {
    let options = Pg2SqliteOptions::default().with_dangling_foreign_keys_allowed();
    assert!(options.is_dangling_foreign_keys_allowed());
}

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
fn forward_reference_translates() {
    let statements = Pg2Sqlite::default()
        .sql(FORWARD_REFERENCE)
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("forward references are order-insensitive and must translate");
    assert!(!statements.is_empty());
}

#[test]
fn opt_out_translates_dangling_table() {
    let options = Pg2SqliteOptions::default().with_dangling_foreign_keys_allowed();
    let sql = Pg2Sqlite::default()
        .sql(DANGLING_TABLE)
        .unwrap()
        .translate_to_sql(&options)
        .expect("opt-out must permit dangling FK targets");
    let joined = sql.join("\n");
    assert!(
        joined.to_lowercase().contains("references"),
        "opt-out must preserve the dead REFERENCES text: {joined}"
    );
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    for s in &sql {
        diesel::sql_query(s.as_str()).execute(&mut conn).unwrap();
    }
}

#[test]
fn opt_out_builds_manifest_for_dangling_table() {
    let options = Pg2SqliteOptions::default().with_dangling_foreign_keys_allowed();
    let manifest = Pg2Sqlite::default()
        .sql(DANGLING_TABLE)
        .unwrap()
        .translation_manifest(&options)
        .expect("opt-out must permit manifest over dangling FK targets");
    assert!(manifest.iter().any(|entry| entry.logical == "child"));
}
