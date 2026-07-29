//! Tests for pgvector to sqlite-vec translation.
//!
//! These tests verify that pgvector types, operators, and DDL are properly
//! translated to sqlite-vec equivalents.
//!
//! # Performance Limitation
//!
//! sqlite-vec v0.1.x uses brute-force search (O(n)), not ANN indexing (O(log
//! n)). The translation is correct, but performance at scale will be slower
//! than pgvector.
//!
//! ANN support is planned: <https://github.com/asg017/sqlite-vec/issues/25>

#![allow(dead_code)]

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[test]
fn test_vector_type_to_blob() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding vector(384)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    // Find the CREATE TABLE statement
    let create_table = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::CreateTable(_)))
        .expect("Should have a CREATE TABLE statement")
        .to_string();

    // The vector column should be BLOB
    assert!(
        create_table.contains("BLOB"),
        "vector(384) should translate to BLOB, got: {create_table}"
    );

    Ok(())
}

#[test]
fn test_halfvec_type_to_blob() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding halfvec(768)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let create_table = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::CreateTable(_)))
        .expect("Should have a CREATE TABLE statement")
        .to_string();

    assert!(
        create_table.contains("BLOB"),
        "halfvec(768) should translate to BLOB, got: {create_table}"
    );

    Ok(())
}

#[test]
fn test_l2_distance_operator() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding BLOB
        );
        SELECT * FROM items ORDER BY embedding <-> '[1,2,3]';
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.contains("vec_distance_L2"),
        "<-> should translate to vec_distance_L2(), got: {select_stmt}"
    );

    Ok(())
}

#[test]
fn test_cosine_distance_operator() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding BLOB
        );
        SELECT * FROM items ORDER BY embedding <=> '[1,2,3]';
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.contains("vec_distance_cosine"),
        "<=> should translate to vec_distance_cosine(), got: {select_stmt}"
    );

    Ok(())
}

#[test]
fn test_vector_cast_to_vec_f32() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding BLOB
        );
        SELECT * FROM items WHERE embedding <-> '[1,2,3]'::vector < 0.5;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.contains("vec_f32"),
        "::vector cast should translate to vec_f32(), got: {select_stmt}"
    );

    Ok(())
}

/// Test that ::halfvec cast is translated to vec_f16() (16-bit float, distinct
/// from vec_f32).
#[test]
fn test_halfvec_cast_to_vec_f16() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding BLOB
        );
        SELECT * FROM items WHERE embedding <=> '[1,2,3]'::halfvec < 0.5;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.contains("vec_f16"),
        "::halfvec cast should translate to vec_f16(), got: {select_stmt}"
    );
    assert!(
        !select_stmt.contains("vec_f32"),
        "::halfvec cast should not translate to vec_f32(), got: {select_stmt}"
    );

    Ok(())
}

#[test]
fn test_vector_column_generates_vec0() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            name TEXT,
            embedding vector(384)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let translated_sql: Vec<_> = translated.iter().map(|s| s.to_string()).collect();

    // Should have: main table + vec0 virtual table + 3 triggers (insert, update,
    // delete)
    assert!(
        translated_sql.len() >= 4,
        "Expected at least 4 statements (table + vec0 + 3 triggers), got: {} statements",
        translated_sql.len()
    );

    // Check for main table
    assert!(
        translated_sql[0].contains("CREATE TABLE items"),
        "First statement should be CREATE TABLE items, got: {}",
        translated_sql[0]
    );

    // Check for vec0 virtual table
    let has_vec0 =
        translated_sql.iter().any(|s| s.contains("CREATE VIRTUAL TABLE") && s.contains("vec0"));
    assert!(has_vec0, "Should have CREATE VIRTUAL TABLE ... USING vec0, got: {translated_sql:?}");

    // Check for triggers
    let has_insert_trigger = translated_sql.iter().any(|s| s.contains("AFTER INSERT ON items"));
    let has_update_trigger = translated_sql.iter().any(|s| s.contains("AFTER UPDATE"));
    let has_delete_trigger = translated_sql.iter().any(|s| s.contains("AFTER DELETE ON items"));

    assert!(has_insert_trigger, "Should have INSERT trigger, got: {translated_sql:?}");
    assert!(has_update_trigger, "Should have UPDATE trigger, got: {translated_sql:?}");
    assert!(has_delete_trigger, "Should have DELETE trigger, got: {translated_sql:?}");

    Ok(())
}

#[test]
fn test_schema_qualified_vector_column_generates_vec0() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            name TEXT,
            embedding public.vector(384)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;
    let translated_sql: Vec<_> = translated.iter().map(|s| s.to_string()).collect();

    let has_vec0 =
        translated_sql.iter().any(|s| s.contains("CREATE VIRTUAL TABLE") && s.contains("vec0"));
    assert!(
        has_vec0,
        "Schema-qualified vector type should still produce vec0 virtual table, got: {translated_sql:?}"
    );

    Ok(())
}

#[test]
fn test_multiple_vector_columns() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            title_embedding vector(384),
            content_embedding vector(768)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let translated_sql: Vec<_> = translated.iter().map(|s| s.to_string()).collect();

    // Check for two vec0 virtual tables
    let vec0_count = translated_sql
        .iter()
        .filter(|s| s.contains("CREATE VIRTUAL TABLE") && s.contains("vec0"))
        .count();

    assert_eq!(vec0_count, 2, "Should have 2 vec0 virtual tables for 2 vector columns");

    // Check for dimension in vec0 definitions
    let has_384 = translated_sql.iter().any(|s| s.contains("float[384]"));
    let has_768 = translated_sql.iter().any(|s| s.contains("float[768]"));

    assert!(has_384, "Should have float[384] for title_embedding");
    assert!(has_768, "Should have float[768] for content_embedding");

    Ok(())
}

#[test]
fn test_no_vector_columns_no_vec0() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            price INTEGER
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    // Should have just the main table
    assert_eq!(translated.len(), 1, "Should have only 1 statement for table without vectors");

    let has_vec0 = translated.iter().any(|s| s.to_string().contains("vec0"));
    assert!(!has_vec0, "Should not have vec0 for table without vector columns");

    Ok(())
}

#[test]
fn test_distance_with_cast() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding BLOB
        );
        SELECT id FROM items WHERE embedding <-> '[1,2,3]'::vector < 1.0
        ORDER BY embedding <-> '[1,2,3]'::vector;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    // Should have vec_distance_L2 and vec_f32
    assert!(
        select_stmt.contains("vec_distance_L2"),
        "Should contain vec_distance_L2, got: {select_stmt}"
    );
    assert!(select_stmt.contains("vec_f32"), "Should contain vec_f32, got: {select_stmt}");

    Ok(())
}

#[test]
fn test_order_by_distance() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding BLOB
        );
        SELECT * FROM items ORDER BY embedding <-> '[0.1,0.2,0.3]'::vector LIMIT 10;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(select_stmt.contains("ORDER BY"), "Should contain ORDER BY, got: {select_stmt}");
    assert!(
        select_stmt.contains("vec_distance_L2"),
        "ORDER BY should use vec_distance_L2, got: {select_stmt}"
    );
    assert!(select_stmt.contains("LIMIT 10"), "Should preserve LIMIT, got: {select_stmt}");

    Ok(())
}

#[test]
fn test_vector_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            embedding vector(384)
        );
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("vector_table_translation", translated_sql);

    Ok(())
}

#[test]
fn test_vector_query_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            embedding BLOB
        );
        SELECT * FROM items
        WHERE embedding <=> '[1,2,3]'::vector < 0.5
        ORDER BY embedding <-> '[1,2,3]'::vector
        LIMIT 5;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("vector_query_translation", translated_sql);

    Ok(())
}
