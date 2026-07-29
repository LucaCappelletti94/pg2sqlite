//! Test translation of UUID related defaults.

use diesel::{prelude::*, sqlite::SqliteConnection};
use pg2sqlite::{pg2sqlite::Pg2Sqlite, prelude::*};

// Schema definitions for test tables
diesel::table! {
    /// Test table for UUID translation.
    users (id) {
        /// UUID primary key.
        id -> Binary,
    }
}

#[derive(Queryable, Selectable, Insertable, Debug)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct User {
    /// UUID primary key.
    id: Vec<u8>,
}

#[declare_sql_function]
extern "SQL" {
    /// Generates a deterministic UUID-like blob for testing.
    fn uuid() -> diesel::sql_types::Binary;
}

fn deterministic_uuid_bytes() -> Vec<u8> {
    vec![
        0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0x4D, 0xEF, 0x80, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
        0x77,
    ]
}

fn uuid_impl() -> Vec<u8> {
    deterministic_uuid_bytes()
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
        "CREATE TABLE users (id BLOB PRIMARY KEY DEFAULT (uuid()) CHECK (length(id) = 16) NOT NULL) STRICT"
    );
}

#[test]
fn uuid_default_uses_registered_uuid_function() {
    let mut conn = SqliteConnection::establish(":memory:").expect("Failed to open SQLite");
    uuid_utils::register_impl(&conn, uuid_impl).expect("Failed to register uuid()");

    let sql = "CREATE TABLE users (id UUID PRIMARY KEY DEFAULT gen_random_uuid())";
    let translated = Pg2Sqlite::default()
        .sql(sql)
        .expect("Failed to parse SQL")
        .translate(&Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob))
        .expect("Failed to translate SQL");

    assert_eq!(translated.len(), 1);
    diesel::sql_query(translated[0].to_string())
        .execute(&mut conn)
        .expect("Failed to create table");

    diesel::sql_query("INSERT INTO users DEFAULT VALUES")
        .execute(&mut conn)
        .expect("Failed to insert default row");

    let rows = users::table
        .select(User::as_select())
        .load::<User>(&mut conn)
        .expect("Failed to fetch inserted rows");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, deterministic_uuid_bytes());
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
