//! Test translation of UUID related defaults.

use pg2sqlite::{pg2sqlite::Pg2Sqlite, prelude::*};

#[test]
fn test_default_gen_random_uuid() {
    let sql = "CREATE TABLE users (id UUID PRIMARY KEY DEFAULT gen_random_uuid())";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let translated = translator
        .translate(&Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob))
        .unwrap();

    assert_eq!(translated.len(), 1);
    let stmt = translated[0].to_string();

    assert_eq!(stmt, "CREATE TABLE users (id BLOB PRIMARY KEY DEFAULT (uuid()) NOT NULL) STRICT");
}
