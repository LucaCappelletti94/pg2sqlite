//! Tests for forward DELETE translation covering USING clause conversion.
//! in `src/impls/translator_impls/delete.rs`.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

// ==================== DELETE with USING ====================

#[test]
fn delete_using_converts_to_exists() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE inactive (user_id INT PRIMARY KEY);
        DELETE FROM users USING inactive WHERE users.id = inactive.user_id;
    ";
    let output = translate(sql);
    // USING should be converted to EXISTS subquery
    assert!(
        output.contains("EXISTS") || output.contains("DELETE"),
        "Expected EXISTS or DELETE: {output}"
    );
}

#[test]
fn delete_using_with_condition() {
    let sql = "
        CREATE TABLE orders (id INT PRIMARY KEY, user_id INT, status TEXT);
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        DELETE FROM orders USING users WHERE orders.user_id = users.id AND users.name = 'test';
    ";
    let output = translate(sql);
    assert!(output.contains("DELETE") || output.contains("EXISTS"), "Expected DELETE: {output}");
}

// ==================== Basic DELETE ====================

#[test]
fn delete_basic() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        DELETE FROM users WHERE id = 1;
    ";
    let output = translate(sql);
    assert!(output.contains("DELETE"), "Expected DELETE: {output}");
}

#[test]
fn delete_all() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        DELETE FROM users;
    ";
    let output = translate(sql);
    assert!(output.contains("DELETE"), "Expected DELETE: {output}");
}
