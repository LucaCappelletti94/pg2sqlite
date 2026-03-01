//! Tests for `Pg2Sqlite` API error paths in `src/pg2sqlite.rs`.
//!
//! Covers:
//! - `sql()` with invalid SQL -> `ParserError`
//! - `file()` with nonexistent path -> `IoError`
//! - `build_schema()` -> returns valid schema
//! - `statement()` builder -> chainable
//! - `reverse_sql()` with invalid SQL -> `ParserError`
//! - `translate()` with empty statements -> empty result

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[test]
fn sql_with_invalid_sql_produces_parser_error() {
    let result = Pg2Sqlite::default().sql("THIS IS NOT VALID SQL !!!");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Parser error"), "Expected parser error, got: {err}");
}

#[test]
fn file_with_nonexistent_path_produces_io_error() {
    let result = Pg2Sqlite::default().file("/nonexistent/path/to/file.sql");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("IO error"), "Expected IO error, got: {err}");
}

#[test]
fn build_schema_returns_valid_schema() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE users (id INT PRIMARY KEY, name TEXT);").unwrap();
    let schema = translator.build_schema();
    assert!(schema.is_ok(), "build_schema should succeed");
}

#[test]
fn build_schema_rejects_non_public_schema_qualification_for_table_refs() {
    let translator = Pg2Sqlite::default()
        .sql(
            "
            CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
            CREATE INDEX idx_users_name ON my_custom_app.users(name);
            ",
        )
        .expect("sql should parse");

    let schema = translator.build_schema();
    assert!(schema.is_err(), "build_schema should reject non-public schema qualification");
}

#[test]
fn build_schema_and_translate_enforce_same_schema_policy() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_users_name ON my_custom_app.users(name);
    ";
    let translator = Pg2Sqlite::default().sql(sql).expect("sql should parse");

    let build_schema_is_err = translator.build_schema().is_err();
    let translate_is_err = translator.translate(&Pg2SqliteOptions::default()).is_err();

    assert_eq!(
        build_schema_is_err, translate_is_err,
        "build_schema and translate should enforce the same schema qualification policy"
    );
}

#[test]
fn statement_builder_is_chainable() {
    // Verify that .statement() returns Self and can be chained
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);";
    let stmts =
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, sql)
            .unwrap();
    let stmt = stmts.into_iter().next().unwrap();

    let translator = Pg2Sqlite::default().statement(stmt);
    let result = translator.translate(&Pg2SqliteOptions::default());
    assert!(result.is_ok());
    assert!(!result.unwrap().is_empty());
}

#[test]
fn translate_with_empty_statements_returns_empty() {
    let translator = Pg2Sqlite::default();
    let result = translator.translate(&Pg2SqliteOptions::default()).unwrap();
    assert!(result.is_empty(), "Empty translator should produce empty result");
}

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

#[test]
fn sql_can_load_multiple_statements() {
    let sql = "CREATE TABLE a (id INT PRIMARY KEY); CREATE TABLE b (id INT PRIMARY KEY);";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let result = translator.translate(&Pg2SqliteOptions::default()).unwrap();
    assert_eq!(result.len(), 2, "Expected 2 statements, got: {}", result.len());
}

#[test]
fn translate_filters_non_translatable_statements() {
    let sql = "
        CREATE TABLE t (id INT PRIMARY KEY);
        CREATE EXTENSION IF NOT EXISTS pgcrypto;
        ALTER TABLE t ADD COLUMN name TEXT;
    ";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let result = translator.translate(&Pg2SqliteOptions::default()).unwrap();
    // Only CREATE TABLE should survive; CREATE EXTENSION and ALTER TABLE are
    // filtered
    assert_eq!(result.len(), 1, "Expected 1 statement, got: {}", result.len());
}

#[test]
fn translate_malformed_trigger_function_body_returns_error_not_panic() {
    let sql = r#"
        CREATE TABLE t (id INT PRIMARY KEY);
        CREATE FUNCTION trg_fn() RETURNS trigger AS $$
            RETURN NEW;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER trg BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION trg_fn();
    "#;
    let translator = Pg2Sqlite::default().sql(sql).unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        translator.translate(&Pg2SqliteOptions::default())
    }));
    assert!(result.is_ok(), "translate should not panic on malformed function body");

    let translation = result.unwrap();
    assert!(
        translation.is_err(),
        "expected a translation error for malformed trigger function body"
    );
}
