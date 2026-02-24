//! Tests verifying that `halfvec` columns map to `vec_f16` in sqlite-vec,
//! distinct from `vector` columns which map to `vec_f32`.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> Result<String, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
        .map_err(|e| e.to_string())
}

#[test]
fn vector_cast_uses_vec_f32() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding vector(3));
               SELECT embedding::vector FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("vec_f32"), "vector cast should use vec_f32, got: {output}");
    assert!(!output.contains("vec_f16"), "vector cast should not use vec_f16, got: {output}");
}

#[test]
fn halfvec_cast_uses_vec_f16() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding halfvec(3));
               SELECT embedding::halfvec FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("vec_f16"), "halfvec cast should use vec_f16, got: {output}");
    assert!(!output.contains("vec_f32"), "halfvec cast should not use vec_f32, got: {output}");
}

#[test]
fn vector_table_uses_float_column_type() {
    let sql = "CREATE TABLE items (id INTEGER PRIMARY KEY, embedding vector(3));";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("float[3]"),
        "vector column in vec0 table should use float[3], got: {output}"
    );
    assert!(
        !output.contains("float16"),
        "vector column should not use float16 type, got: {output}"
    );
}

#[test]
fn halfvec_table_uses_float16_column_type() {
    let sql = "CREATE TABLE items (id INTEGER PRIMARY KEY, embedding halfvec(3));";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("float16[3]"),
        "halfvec column in vec0 table should use float16[3], got: {output}"
    );
    assert!(
        !output.contains("float[3]"),
        "halfvec column should not use float[3] type, got: {output}"
    );
}

#[test]
fn table_with_both_vector_and_halfvec_columns() {
    let sql = "CREATE TABLE items (
        id INTEGER PRIMARY KEY,
        embedding vector(3),
        half_embedding halfvec(3)
    );";
    let output = translate(sql).unwrap();
    assert!(output.contains("float[3]"), "vector column should use float[3], got: {output}");
    assert!(output.contains("float16[3]"), "halfvec column should use float16[3], got: {output}");
}
