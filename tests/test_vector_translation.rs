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

use std::sync::Once;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::{Connection, functions::FunctionFlags};
use sqlite_vec::sqlite3_vec_init;

/// Register sqlite-vec once per process via `sqlite3_auto_extension`.
///
/// SAFETY: `sqlite3_vec_init` is the sqlite-vec C entry point whose signature
/// is `(db, pzErrMsg, pApi) -> int`. The transmute restores that type so the
/// C API can store and call it. `Once` makes the registration single-shot.
fn register_sqlite_vec_once() {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(sqlite3_vec_init as *const ())));
    });
}

/// Convert a single `f32` to a 2-byte little-endian IEEE 754 half-precision
/// value. Used to implement `vec_f16`, which sqlite-vec 0.1.9 does not ship.
#[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn f32_to_f16_le(x: f32) -> [u8; 2] {
    let b: u32 = x.to_bits();
    // Extract sign (1 bit), biased exponent (8 bits), mantissa (23 bits).
    // The masked sub-fields are provably bounded; debug_asserts document that.
    let sign: u16 = {
        debug_assert!(b >> 31 <= 1);
        (b >> 31) as u16 // deliberate: single-bit extraction, value 0 or 1
    } << 15;
    let exp32: i32 = {
        let e = (b >> 23) & 0xFF;
        debug_assert!(e <= 255);
        e as i32 // deliberate: 8-bit field, always 0..=255, fits i32
    };
    let mantissa: u32 = b & 0x7F_FFFF;
    let bits: u16 = if exp32 == 0xFF {
        let top10: u16 = {
            let m = mantissa >> 13;
            debug_assert!(m <= 0x3FF);
            m as u16 // deliberate: top-10 mantissa bits, value <= 0x3FF
        };
        if mantissa != 0 { 0x7E00 | sign | top10 } else { 0x7C00 | sign }
    } else if exp32 == 0 {
        sign
    } else {
        let e = exp32 - 127 + 15;
        if e >= 31 {
            0x7C00 | sign
        } else if e <= 0 {
            sign
        } else {
            debug_assert!(e > 0 && e <= 30);
            debug_assert!(mantissa >> 13 <= 0x3FF);
            let e16: u16 = e as u16; // deliberate: proven 1..=30, fits u16
            let m16: u16 = (mantissa >> 13) as u16; // deliberate: <= 0x3FF, fits u16
            sign | (e16 << 10) | m16
        }
    };
    bits.to_le_bytes()
}

/// Register a `vec_f16` scalar function on `conn`.
///
/// sqlite-vec 0.1.9 does not provide this function. We supply it here so tests
/// that verify halfvec translation can also execute the emitted SQL.
/// rusqlite is used directly because diesel does not expose
/// `create_scalar_function`.
fn register_vec_f16(conn: &Connection) {
    conn.create_scalar_function(
        "vec_f16",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            use rusqlite::types::ValueRef;
            match ctx.get_raw(0) {
                ValueRef::Null => Ok(rusqlite::types::Value::Null),
                ValueRef::Text(t) => {
                    let text = String::from_utf8_lossy(t);
                    let trimmed = text.trim().trim_start_matches('[').trim_end_matches(']');
                    let bytes: Vec<u8> = trimmed
                        .split(',')
                        .filter_map(|s| s.trim().parse::<f32>().ok())
                        .flat_map(f32_to_f16_le)
                        .collect();
                    Ok(rusqlite::types::Value::Blob(bytes))
                }
                _ => {
                    Err(rusqlite::Error::InvalidFunctionParameterType(
                        0,
                        rusqlite::types::Type::Text,
                    ))
                }
            }
        },
    )
    .expect("register vec_f16");
}

/// Open an in-memory SQLite connection with sqlite-vec loaded and `vec_f16`
/// registered. rusqlite is used directly because extension registration and
/// custom function registration require APIs that diesel does not expose.
fn open_vec_conn() -> Connection {
    register_sqlite_vec_once();
    let conn = Connection::open_in_memory().expect("in-memory SQLite");
    register_vec_f16(&conn);
    conn
}

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
    // Translated DDL is dynamically generated; rusqlite execute_batch proves
    // SQLite accepts it.
    rusqlite::Connection::open_in_memory()
        .expect("in-memory SQLite")
        .execute_batch(&format!("{create_table};"))
        .unwrap_or_else(|e| panic!("SQLite rejected CREATE TABLE: {e}\n{create_table}"));

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
    // Translated DDL is dynamically generated; rusqlite execute_batch proves
    // SQLite accepts it.
    rusqlite::Connection::open_in_memory()
        .expect("in-memory SQLite")
        .execute_batch(&format!("{create_table};"))
        .unwrap_or_else(|e| panic!("SQLite rejected CREATE TABLE: {e}\n{create_table}"));

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
    // Execute all emitted statements with sqlite-vec loaded so vec_distance_L2
    // is available at prepare time.
    let conn = open_vec_conn();
    for stmt in &translated {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("SQLite rejected statement: {e}\n{stmt}"));
    }

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
    // Execute all emitted statements with sqlite-vec loaded.
    let conn = open_vec_conn();
    for stmt in &translated {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("SQLite rejected statement: {e}\n{stmt}"));
    }

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
    // Execute all emitted statements with sqlite-vec loaded so vec_f32 is
    // available.
    let conn = open_vec_conn();
    for stmt in &translated {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("SQLite rejected statement: {e}\n{stmt}"));
    }

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
    // Execute all emitted statements with sqlite-vec and vec_f16 loaded.
    let conn = open_vec_conn();
    for stmt in &translated {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("SQLite rejected statement: {e}\n{stmt}"));
    }

    Ok(())
}

/// The schema-qualified spelling of the same cast. pgvector commonly lives in
/// a named schema, and a qualified type cast is ordinary PostgreSQL, so
/// `'...'::public.vector` must lower exactly like `'...'::vector`. It used to
/// fall through to `CAST('[1,2]' AS BLOB)`, which applies cleanly and stores
/// the text bytes as the vector.
#[test]
fn qualified_vector_cast_lowers_to_vec_f32() -> Result<(), Box<dyn std::error::Error>> {
    let translated = Pg2Sqlite::default()
        .sql("SELECT '[1,2]'::public.vector AS v;")?
        .translate(&Pg2SqliteOptions::default())?;
    let select_stmt = translated[0].to_string();

    assert!(
        select_stmt.contains("vec_f32('[1,2]')"),
        "a qualified ::vector cast should lower to vec_f32(), got: {select_stmt}"
    );
    assert!(
        !select_stmt.to_uppercase().contains("AS BLOB"),
        "the CAST AS BLOB fallback stores text bytes as the vector, got: {select_stmt}"
    );
    // Execute the standalone SELECT with sqlite-vec loaded so vec_f32 is known.
    open_vec_conn()
        .execute_batch(&format!("{select_stmt};"))
        .unwrap_or_else(|e| panic!("SQLite rejected SELECT: {e}\n{select_stmt}"));
    Ok(())
}

/// The halfvec twin, which has its own copy of the same predicate.
#[test]
fn qualified_halfvec_cast_lowers_to_vec_f16() -> Result<(), Box<dyn std::error::Error>> {
    let translated = Pg2Sqlite::default()
        .sql("SELECT '[1,2]'::public.halfvec AS v;")?
        .translate(&Pg2SqliteOptions::default())?;
    let select_stmt = translated[0].to_string();

    assert!(
        select_stmt.contains("vec_f16('[1,2]')"),
        "a qualified ::halfvec cast should lower to vec_f16(), got: {select_stmt}"
    );
    // Execute the standalone SELECT with vec_f16 registered so it is known.
    open_vec_conn()
        .execute_batch(&format!("{select_stmt};"))
        .unwrap_or_else(|e| panic!("SQLite rejected SELECT: {e}\n{select_stmt}"));
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

    // Should have: main table + vec0 virtual table + 3 triggers (insert,
    // update, delete)
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
    // Execute non-vec0 statements; vec0 requires the sqlite-vec extension which
    // is unavailable in the test environment, so vec0 DDL is skipped.
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for s in &translated_sql {
        if !s.to_ascii_uppercase().contains("VEC0") {
            conn.execute_batch(&format!("{s};"))
                .unwrap_or_else(|e| panic!("SQLite rejected emitted SQL: {e}\n{s}"));
        }
    }

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
    // Execute non-vec0 statements; vec0 requires the sqlite-vec extension which
    // is unavailable in the test environment, so vec0 DDL is skipped.
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for s in &translated_sql {
        if !s.to_ascii_uppercase().contains("VEC0") {
            conn.execute_batch(&format!("{s};"))
                .unwrap_or_else(|e| panic!("SQLite rejected emitted SQL: {e}\n{s}"));
        }
    }

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
    // Execute all emitted statements with sqlite-vec loaded.
    let conn = open_vec_conn();
    for stmt in &translated {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("SQLite rejected statement: {e}\n{stmt}"));
    }

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
    // Execute all emitted statements with sqlite-vec loaded.
    let conn = open_vec_conn();
    for stmt in &translated {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("SQLite rejected statement: {e}\n{stmt}"));
    }

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
