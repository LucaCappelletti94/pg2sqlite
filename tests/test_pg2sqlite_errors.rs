//! Tests for `Pg2Sqlite` API error paths in `src/pg2sqlite.rs`.
//!
//! Covers:
//! - `sql()` with invalid SQL -> `ParserError`
//! - `file()` with nonexistent path -> `IoError`
//! - `build_schema()` -> returns valid schema
//! - `statement()` builder -> chainable
//! - `reverse_sql()` with invalid SQL -> `ParserError`
//! - `translate()` with empty statements -> empty result

use pg2sqlite::{
    errors::{Error, RefusalCategory, TranslationDirection},
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};

#[test]
fn sql_with_invalid_sql_produces_parser_error() {
    let input = "THIS IS NOT VALID SQL !!!";
    let error = Pg2Sqlite::default().sql(input).expect_err("invalid SQL must fail");
    let Error::SqlParse(parse_error) = error else {
        panic!("expected owned parse error");
    };
    assert_eq!(parse_error.input(), input);
    assert!(std::error::Error::source(&parse_error).is_some());
}

#[cfg(feature = "std")]
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
fn build_schema_rejects_index_on_unknown_schema_qualified_table() {
    // sql-traits HEAD (no_std refactor) is strict during schema construction:
    // an index referencing `my_custom_app.users` while the only `users` table
    // is unqualified surfaces as TableNotFoundForIndex at build_schema time
    // rather than only at translate time.
    let translator = Pg2Sqlite::default()
        .sql(
            "
            CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
            CREATE INDEX idx_users_name ON my_custom_app.users(name);
            ",
        )
        .expect("sql should parse");

    let err =
        translator.build_schema().expect_err("build_schema should reject mismatched index target");
    assert!(
        err.to_string().contains("TableNotFoundForIndex") || err.to_string().contains("users"),
        "expected schema-side TableNotFoundForIndex error, got: {err}"
    );
}

#[test]
fn build_schema_allows_non_public_schema_qualified_create_table() {
    let translator = Pg2Sqlite::default()
        .sql("CREATE SCHEMA my_custom_app; CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);")
        .expect("sql should parse");

    let schema = translator.build_schema();
    assert!(schema.is_ok(), "build_schema should accept schema-qualified CREATE TABLE");
}

#[test]
fn build_schema_and_translate_both_reject_mismatched_index_schema() {
    // Under sql-traits HEAD, schema validation for index targets fires at
    // build_schema time, so build_schema and translate now both reject the
    // same SQL instead of diverging at the policy boundary.
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_users_name ON my_custom_app.users(name);
    ";
    let translator = Pg2Sqlite::default().sql(sql).expect("sql should parse");

    assert!(
        translator.build_schema().is_err(),
        "build_schema should reject mismatched index target"
    );
    assert!(
        translator.translate(&Pg2SqliteOptions::default()).is_err(),
        "translate should also reject mismatched index target"
    );
}

#[test]
fn build_schema_and_translate_match_for_non_public_create_table() {
    let sql = "CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);";
    let translator = Pg2Sqlite::default().sql(sql).expect("sql should parse");

    let build_schema_is_err = translator.build_schema().is_err();
    let translate_is_err = translator.translate(&Pg2SqliteOptions::default()).is_err();

    assert_eq!(
        build_schema_is_err, translate_is_err,
        "build_schema and translate should enforce the same policy for create table"
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
    let statements = result.unwrap();
    assert_ne!(statements.len(), 0, "statement() translation must produce at least one statement");
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
    let input = "NOT VALID SQL !!!";

    let error =
        translator.reverse_sql(input, &schema, &options).expect_err("invalid SQL must fail");
    let Error::SqlParse(parse_error) = error else {
        panic!("expected owned parse error");
    };
    assert_eq!(parse_error.input(), input);
    assert!(std::error::Error::source(&parse_error).is_some());
}

#[test]
fn refusals_expose_direction_and_stable_category() {
    let forward = Pg2Sqlite::default()
        .sql("SELECT sqrt(2.0)")
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .expect_err("math functions require an explicit destination capability");
    let Error::TranslationRefusal(forward) = forward else {
        panic!("expected a forward refusal");
    };
    assert_eq!(forward.direction(), TranslationDirection::PostgreSqlToSqlite);
    assert_eq!(forward.category(), RefusalCategory::UnrepresentableSemantics);

    let translator = Pg2Sqlite::default().sql("CREATE TABLE t (id INT PRIMARY KEY);").unwrap();
    let schema = translator.build_schema().unwrap();
    let reverse = translator
        .reverse_sql("SELECT rowid FROM t", &schema, &Pg2SqliteOptions::default())
        .expect_err("SQLite rowid has no PostgreSQL equivalent");
    let Error::TranslationRefusal(reverse) = reverse else {
        panic!("expected a reverse refusal");
    };
    assert_eq!(reverse.direction(), TranslationDirection::SqliteToPostgreSql);
    assert_eq!(reverse.category(), RefusalCategory::UnrepresentableSemantics);
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
    // CREATE TABLE and ALTER TABLE survive. Only CREATE EXTENSION is filtered:
    // SQLite supports ADD COLUMN, so it is translated rather than dropped.
    assert_eq!(result.len(), 2, "Expected 2 statements, got: {}", result.len());
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
