//! Tests for explicit UUID version overrides (gen_random_uuid/uuidv4 -> V4,
//! uuidv7 -> V7).

use pg2sqlite::{pg2sqlite::Pg2Sqlite, prelude::*};

#[test]
fn test_explicit_uuidv4_overrides_default_v7() {
    let sql = "CREATE TABLE t (id UUID DEFAULT uuidv4())";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let options = Pg2SqliteOptions::default()
        .use_pure_sql_for_uuid(true)
        .with_uuid_representation(UuidRepresentation::Text);

    let translated = translator.translate(&options).unwrap();
    let stmt = translated[0].to_string();

    assert!(
        stmt.contains("printf('%04x', (abs(random()) & 4095) | 16384)"),
        "Should contain V4 version bits"
    );
    assert!(!stmt.contains("julianday"), "Should NOT contain julianday (V7 feature)");
}

#[test]
fn test_explicit_gen_random_uuid_overrides_default_v7() {
    // gen_random_uuid is V4. Even if default is V7.
    let sql = "CREATE TABLE t (id UUID DEFAULT gen_random_uuid())";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let options = Pg2SqliteOptions::default()
        .use_pure_sql_for_uuid(true)
        .with_uuid_representation(UuidRepresentation::Text);

    let translated = translator.translate(&options).unwrap();
    let stmt = translated[0].to_string();

    assert!(
        stmt.contains("printf('%04x', (abs(random()) & 4095) | 16384)"),
        "Should contain V4 version bits"
    );
}

#[test]
fn test_explicit_uuidv7_overrides_default_v4() {
    let sql = "CREATE TABLE t (id UUID DEFAULT uuidv7())";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let options = Pg2SqliteOptions::default()
        .use_pure_sql_for_uuid(true)
        .with_uuid_representation(UuidRepresentation::Text);

    let translated = translator.translate(&options).unwrap();
    let stmt = translated[0].to_string();

    assert!(stmt.contains("julianday"), "Should contain julianday (V7 feature)");
    // V7 version bits: 0x7000
    assert!(stmt.contains("28672"), "Should contain V7 version bits (28672 = 0x7000)");
}
