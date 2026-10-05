//! Tests verifying that `halfvec` columns map to `vec_f16` in sqlite-vec,
//! distinct from `vector` columns which map to `vec_f32`.

mod helpers;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[test]
fn vector_cast_uses_vec_f32() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, embedding vector(3));
               SELECT embedding::vector FROM t;";
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    let output = stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(output.contains("vec_f32"), "vector cast should use vec_f32, got: {output}");
    assert!(!output.contains("vec_f16"), "vector cast should not use vec_f16, got: {output}");
    let conn = helpers::vec_connection();
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
    let conn = helpers::vec_connection();
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
    let conn = helpers::vec_connection();
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
    let conn = helpers::vec_connection();
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
    let conn = helpers::vec_connection();
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap();
    }
}
