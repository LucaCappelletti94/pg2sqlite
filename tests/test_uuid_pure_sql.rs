//! Test pure SQL UUID translation.

use pg2sqlite::{pg2sqlite::Pg2Sqlite, prelude::*};

#[test]
fn test_pure_sql_uuid_v4_text() {
    let sql = "CREATE TABLE t_v4 (id UUID PRIMARY KEY DEFAULT gen_random_uuid())";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let options = Pg2SqliteOptions::default()
        .use_pure_sql_for_uuid()
        .with_uuid_representation(UuidRepresentation::Text);

    let translated = translator.translate(&options).unwrap();
    let stmt = translated[0].to_string();

    // We expect TEXT type and pure SQL default
    assert!(stmt.contains("id TEXT PRIMARY KEY"));
    assert!(stmt.contains("lower(hex(randomblob(4))"));
    // Ensure it is wrapped in parens
    assert!(stmt.contains("DEFAULT (lower("));
}

#[test]
fn test_pure_sql_uuid_v4_blob() {
    let sql = "CREATE TABLE t_v4_bin (id UUID PRIMARY KEY DEFAULT gen_random_uuid())";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let options = Pg2SqliteOptions::default()
        .use_pure_sql_for_uuid()
        .with_uuid_representation(UuidRepresentation::Blob);

    let translated = translator.translate(&options).unwrap();
    let stmt = translated[0].to_string();

    assert!(stmt.contains("id BLOB PRIMARY KEY"));
    // Check for some signature parts of the blob expression
    assert!(stmt.contains("randomblob(6)"));
    assert!(stmt.contains("unhex"));
    // We moved to using unhex instead of char for safety
}

#[test]
fn test_pure_sql_uuid_v7_text() {
    let sql = "CREATE TABLE t_v7 (id UUID PRIMARY KEY DEFAULT uuidv7())";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let options = Pg2SqliteOptions::default()
        .use_pure_sql_for_uuid()
        .with_uuid_representation(UuidRepresentation::Text);

    let translated = translator.translate(&options).unwrap();
    let stmt = translated[0].to_string();

    assert!(stmt.contains("id TEXT PRIMARY KEY"));
    // Check V7 specific julianday usage
    assert!(stmt.contains("julianday('now')"));
}

#[test]
fn test_pure_sql_uuid_v7_blob() {
    let sql = "CREATE TABLE t_v7_bin (id UUID PRIMARY KEY DEFAULT uuidv7())";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let options = Pg2SqliteOptions::default()
        .use_pure_sql_for_uuid()
        .with_uuid_representation(UuidRepresentation::Blob);

    let translated = translator.translate(&options).unwrap();
    let stmt = translated[0].to_string();

    assert!(stmt.contains("id BLOB PRIMARY KEY"));
    // Check V7 blob specific usage (we used unhex)
    // Note: sqlparser might uppercase SUBSTR
    let stmt_lower = stmt.to_lowercase();
    assert!(stmt_lower.contains("unhex(substr"));
    assert!(stmt_lower.contains("julianday('now')"));
}

#[test]
fn test_uuid_requires_representation() {
    let sql = "CREATE TABLE t_fail (id UUID PRIMARY KEY)";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let options = Pg2SqliteOptions::default(); // No representation specified

    let result = translator.translate(&options);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("UUID translation requires specifying a representation")
    );
}
