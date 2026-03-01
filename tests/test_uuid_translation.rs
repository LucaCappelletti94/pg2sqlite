//! Test translation of UUID related defaults.

use diesel::prelude::*;
use pg2sqlite::{pg2sqlite::Pg2Sqlite, prelude::*};

// Schema definitions for test tables
diesel::table! {
    /// Test table for UUID translation.
    users (id) {
        /// UUID primary key.
        id -> Binary,
    }
}

/// User record with UUID.
#[derive(Queryable, Selectable, Insertable, Debug)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct User {
    /// UUID primary key.
    id: Vec<u8>,
}

#[test]
fn test_default_gen_random_uuid() {
    let sql = "CREATE TABLE users (id UUID PRIMARY KEY DEFAULT gen_random_uuid())";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let translated = translator
        .translate(&Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob))
        .unwrap();

    assert_eq!(translated.len(), 1);
    let stmt = translated[0].to_string();

    assert_eq!(
        stmt,
        "CREATE TABLE users (id BLOB PRIMARY KEY DEFAULT (randomblob(16)) NOT NULL) STRICT"
    );
}

#[test]
fn uuid_without_repr_still_errors() {
    let sql = "CREATE TABLE t (id UUID PRIMARY KEY);";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "UUID without representation should error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("UUID translation requires"),
        "Expected UUID representation error, got: {err}"
    );
}
