//! Test checks for enforcing NOT NULL on primary keys.

use pg2sqlite::{pg2sqlite::Pg2Sqlite, prelude::*};

#[test]
fn test_pk_simple_int_nullable() {
    let sql = "CREATE TABLE test_int (id INT PRIMARY KEY)";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let translated = translator.translate(&Pg2SqliteOptions::default()).unwrap();
    assert_eq!(translated.len(), 1);
    assert_eq!(
        translated[0].to_string(),
        "CREATE TABLE test_int (id INTEGER PRIMARY KEY NOT NULL) STRICT"
    );
}

#[test]
fn test_pk_simple_int_not_null() {
    let sql = "CREATE TABLE test_int_nn (id INT PRIMARY KEY NOT NULL)";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let translated = translator.translate(&Pg2SqliteOptions::default()).unwrap();
    assert_eq!(translated.len(), 1);
    assert_eq!(
        translated[0].to_string(),
        "CREATE TABLE test_int_nn (id INTEGER PRIMARY KEY NOT NULL) STRICT"
    );
}

#[test]
fn test_pk_string_binary_nullable() {
    let sql = "
    CREATE TABLE test_text (id TEXT PRIMARY KEY);
    CREATE TABLE test_blob (id BYTEA PRIMARY KEY);
    ";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let translated = translator.translate(&Pg2SqliteOptions::default()).unwrap();
    assert_eq!(translated.len(), 2);
    assert_eq!(
        translated[0].to_string(),
        "CREATE TABLE test_text (id TEXT PRIMARY KEY NOT NULL) STRICT"
    );
    assert_eq!(
        translated[1].to_string(),
        "CREATE TABLE test_blob (id BLOB PRIMARY KEY NOT NULL) STRICT"
    );
}

#[test]
fn test_pk_composite() {
    let sql = "
    CREATE TABLE test_composite (
        id1 INT,
        id2 TEXT,
        PRIMARY KEY (id1, id2)
    );
    ";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let translated = translator.translate(&Pg2SqliteOptions::default()).unwrap();
    assert_eq!(translated.len(), 1);
    assert_eq!(
        translated[0].to_string(),
        "CREATE TABLE test_composite (id1 INTEGER NOT NULL, id2 TEXT NOT NULL, PRIMARY KEY (id1, id2)) STRICT"
    );
}
