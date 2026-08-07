//! Tests verifying that `halfvec` columns map to `vec_f16` in sqlite-vec,
//! distinct from `vector` columns which map to `vec_f32`.

use std::sync::Once;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::{Connection, ffi::sqlite3_auto_extension, functions::FunctionFlags};
use sqlite_vec::sqlite3_vec_init;

/// Convert a single `f32` to a 2-byte little-endian IEEE 754 half-precision
/// value. sqlite-vec 0.1.9 does not ship `vec_f16`, so we provide it.
#[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn f32_to_f16_le(x: f32) -> [u8; 2] {
    let b: u32 = x.to_bits();
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

/// Register the sqlite-vec extension and `vec_f16` once, then open a fresh
/// in-memory connection with both available.
///
/// rusqlite is used directly because diesel does not expose
/// `sqlite3_auto_extension` or `create_scalar_function`.
fn sqlite_vec_connection() -> Connection {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // SAFETY: sqlite3_vec_init has the C function signature that
        // sqlite3_auto_extension expects. The transmute reinterprets the
        // function pointer type so the C API can store and call it.
        unsafe {
            sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut i8,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> i32,
            >(sqlite3_vec_init as *const ())));
        }
    });
    let conn = Connection::open_in_memory().expect("open in-memory SQLite with sqlite-vec");
    // sqlite-vec 0.1.9 does not provide vec_f16; register it per-connection.
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
    conn
}

#[test]
fn vector_cast_uses_vec_f32() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding vector(3));
               SELECT embedding::vector FROM t;";
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    let output = stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(output.contains("vec_f32"), "vector cast should use vec_f32, got: {output}");
    assert!(!output.contains("vec_f16"), "vector cast should not use vec_f16, got: {output}");
    let conn = sqlite_vec_connection();
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap();
    }
}

#[test]
fn halfvec_cast_uses_vec_f16() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding halfvec(3));
               SELECT embedding::halfvec FROM t;";
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    let output = stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(output.contains("vec_f16"), "halfvec cast should use vec_f16, got: {output}");
    assert!(!output.contains("vec_f32"), "halfvec cast should not use vec_f32, got: {output}");
    let conn = sqlite_vec_connection();
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap();
    }
}

#[test]
fn vector_table_uses_float_column_type() {
    let sql = "CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));";
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    let output = stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(
        output.contains("float[3]"),
        "vector column in vec0 table should use float[3], got: {output}"
    );
    assert!(
        !output.contains("float16"),
        "vector column should not use float16 type, got: {output}"
    );
    let conn = sqlite_vec_connection();
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap();
    }
}

#[test]
fn halfvec_table_uses_float16_column_type() {
    let sql = "CREATE TABLE items (id INTEGER PRIMARY KEY, embedding halfvec(3));";
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    let output = stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(
        output.contains("float16[3]"),
        "halfvec column in vec0 table should use float16[3], got: {output}"
    );
    assert!(
        !output.contains("float[3]"),
        "halfvec column should not use float[3] type, got: {output}"
    );
    let conn = sqlite_vec_connection();
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap();
    }
}

#[test]
fn table_with_both_vector_and_halfvec_columns() {
    let sql = "CREATE TABLE items (
        id INTEGER PRIMARY KEY,
        embedding vector(3),
        half_embedding halfvec(3)
    );";
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    let output = stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(output.contains("float[3]"), "vector column should use float[3], got: {output}");
    assert!(output.contains("float16[3]"), "halfvec column should use float16[3], got: {output}");
    let conn = sqlite_vec_connection();
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap();
    }
}
