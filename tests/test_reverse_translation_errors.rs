//! Tests for reverse translation error paths in
//! `src/impls/reverse_translator_impls/`.
//!
//! Covers:
//! - RLS table in JOIN -> error
//! - RLS table in subquery -> error
//! - RLS table in UPDATE FROM -> error
//! - RLS table in DELETE USING -> error
//! - RLS table with custom suffix -> error
//! - Multiple statements via reverse_sql() -> all translated
//! - Parser error in reverse_sql -> ParserError variant

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};

// ==================== RLS table detection ====================

#[test]
fn rls_table_in_select_produces_error() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator.reverse_sql("SELECT * FROM users_rls;", &schema, &options);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("users_rls") && err.contains("RLS"),
        "Expected RLS table detected error, got: {err}"
    );
}

#[test]
fn rls_table_in_join_produces_error() {
    let translator = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
             CREATE TABLE posts (id INT PRIMARY KEY, author_id INT REFERENCES users(id), title TEXT);",
        )
        .unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator.reverse_sql(
        "SELECT * FROM users JOIN users_rls ON users.id = users_rls.id;",
        &schema,
        &options,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("users_rls") && err.contains("RLS"),
        "Expected RLS table detected in JOIN, got: {err}"
    );
}

#[test]
fn rls_table_in_subquery_produces_error() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator.reverse_sql(
        "SELECT * FROM (SELECT * FROM users_rls) AS sub;",
        &schema,
        &options,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("users_rls") && err.contains("RLS"),
        "Expected RLS table detected in subquery, got: {err}"
    );
}

#[test]
fn rls_table_in_insert_source_produces_error() {
    let translator = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
             CREATE TABLE archive (id INT PRIMARY KEY, name TEXT);",
        )
        .unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result =
        translator.reverse_sql("INSERT INTO archive SELECT * FROM users_rls;", &schema, &options);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("users_rls") && err.contains("RLS"),
        "Expected RLS table detected in INSERT source, got: {err}"
    );
}

#[test]
fn rls_table_in_insert_target_produces_error() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator.reverse_sql(
        "INSERT INTO users_rls (id, name) VALUES (1, 'test');",
        &schema,
        &options,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("users_rls") && err.contains("RLS"),
        "Expected RLS table detected in INSERT target, got: {err}"
    );
}

#[test]
fn rls_table_in_update_target_produces_error() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator.reverse_sql(
        "UPDATE users_rls SET name = 'test' WHERE id = 1;",
        &schema,
        &options,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("users_rls") && err.contains("RLS"),
        "Expected RLS table detected in UPDATE target, got: {err}"
    );
}

#[test]
fn rls_table_in_delete_produces_error() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator.reverse_sql("DELETE FROM users_rls WHERE id = 1;", &schema, &options);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("users_rls") && err.contains("RLS"),
        "Expected RLS table detected in DELETE, got: {err}"
    );
}

#[test]
fn rls_table_in_set_operation_produces_error() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator.reverse_sql(
        "SELECT id, name FROM users UNION SELECT id, name FROM users_rls;",
        &schema,
        &options,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("users_rls") && err.contains("RLS"),
        "Expected RLS table detected in set operation, got: {err}"
    );
}

#[test]
fn rls_table_in_insert_source_set_operation_produces_error() {
    let translator = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
             CREATE TABLE archive (id INT PRIMARY KEY, name TEXT);",
        )
        .unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator.reverse_sql(
        "INSERT INTO archive (id, name)
         SELECT id, name FROM users
         UNION
         SELECT id, name FROM users_rls;",
        &schema,
        &options,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("users_rls") && err.contains("RLS"),
        "Expected RLS table detected in INSERT set operation source, got: {err}"
    );
}

// ==================== RLS table in expression subqueries ====================

#[test]
fn rls_table_in_where_subquery_produces_error() {
    let translator = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
             CREATE TABLE orders (id INT PRIMARY KEY, user_id INT);",
        )
        .unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator.reverse_sql(
        "SELECT * FROM orders WHERE EXISTS (SELECT 1 FROM users_rls WHERE users_rls.id = orders.user_id);",
        &schema,
        &options,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("users_rls") && err.contains("RLS"),
        "Expected RLS table detected in WHERE subquery, got: {err}"
    );
}

#[test]
fn rls_table_in_having_subquery_produces_error() {
    let translator = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
             CREATE TABLE orders (id INT PRIMARY KEY, user_id INT);",
        )
        .unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator.reverse_sql(
        "SELECT user_id, COUNT(*) FROM orders GROUP BY user_id HAVING COUNT(*) > (SELECT COUNT(*) FROM users_rls);",
        &schema,
        &options,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("users_rls") && err.contains("RLS"),
        "Expected RLS table detected in HAVING subquery, got: {err}"
    );
}

#[test]
fn rls_table_in_projection_subquery_produces_error() {
    let translator = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
             CREATE TABLE orders (id INT PRIMARY KEY, user_id INT);",
        )
        .unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator.reverse_sql(
        "SELECT id, (SELECT name FROM users_rls WHERE users_rls.id = orders.user_id) AS user_name FROM orders;",
        &schema,
        &options,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("users_rls") && err.contains("RLS"),
        "Expected RLS table detected in projection subquery, got: {err}"
    );
}

// ==================== Custom RLS suffix ====================

#[test]
fn rls_table_with_custom_suffix_produces_error() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default().with_rls_table_suffix("_backing");

    let result = translator.reverse_sql("SELECT * FROM users_backing;", &schema, &options);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("users_backing") && err.contains("_backing"),
        "Expected RLS table detected with custom suffix, got: {err}"
    );
}

// ==================== Multiple statements ====================

#[test]
fn reverse_sql_translates_multiple_statements() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator
        .reverse_sql("SELECT * FROM users; SELECT id FROM users WHERE id = 1;", &schema, &options)
        .unwrap();
    assert_eq!(result.len(), 2, "Expected 2 translated statements, got: {}", result.len());
}

// ==================== Parser error ====================

#[test]
fn reverse_sql_with_invalid_sql_produces_parser_error() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator.reverse_sql("NOT VALID SQL !!!", &schema, &options);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Parser error"), "Expected parser error, got: {err}");
}

// ==================== Unsupported reverse statement ====================

#[test]
fn reverse_translate_non_dml_produces_error() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result =
        translator.reverse_sql("CREATE TABLE foo (id INT PRIMARY KEY);", &schema, &options);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Reverse translation only supports DML"),
        "Expected unsupported reverse statement error, got: {err}"
    );
}

// ==================== Valid reverse translations ====================

#[test]
fn reverse_translate_select_succeeds() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator.reverse_sql("SELECT * FROM users;", &schema, &options).unwrap();
    assert_eq!(result.len(), 1);
    let output = result[0].to_string();
    assert!(output.contains("SELECT"), "Expected SELECT in output, got: {output}");
}

#[test]
fn reverse_translate_insert_succeeds() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator
        .reverse_sql("INSERT INTO users (id, name) VALUES (1, 'test');", &schema, &options)
        .unwrap();
    assert_eq!(result.len(), 1);
}

#[test]
fn reverse_translate_delete_succeeds() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result =
        translator.reverse_sql("DELETE FROM users WHERE id = 1;", &schema, &options).unwrap();
    assert_eq!(result.len(), 1);
}

#[test]
fn reverse_translate_update_succeeds() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();

    let result = translator
        .reverse_sql("UPDATE users SET name = 'updated' WHERE id = 1;", &schema, &options)
        .unwrap();
    assert_eq!(result.len(), 1);
}
