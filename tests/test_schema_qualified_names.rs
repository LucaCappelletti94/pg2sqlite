//! Tests for schema-qualified PostgreSQL object names being normalized for
//! SQLite output.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, Translator};
use rusqlite::Connection;
use sql_traits::structs::ParserDB;
use sqlparser::{ast::Statement, dialect::PostgreSqlDialect, parser::Parser};

fn translate(sql: &str) -> Result<Vec<Statement>, Box<dyn std::error::Error>> {
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    Ok(translated)
}

fn translated_sql(sql: &str) -> String {
    translate(sql)
        .expect("translation should succeed")
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn translate_single_statement(sql: &str) -> String {
    let mut statements = Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse");
    let statement = statements.remove(0);
    let schema =
        ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build");
    let translated = statement
        .translate(&schema, &Pg2SqliteOptions::default())
        .expect("single statement translation should succeed");
    translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
}

#[test]
fn create_table_schema_qualified_name_is_unqualified() {
    let output = translated_sql("CREATE TABLE public.users (id INT PRIMARY KEY, name TEXT);");
    assert!(
        output.contains("CREATE TABLE users"),
        "expected unqualified table name, got: {output}"
    );
    assert!(
        !output.contains("public.users"),
        "schema qualifier should be removed for SQLite, got: {output}"
    );
}

#[test]
fn create_view_schema_qualified_names_are_unqualified() {
    let output = translated_sql(
        "
        CREATE TABLE public.users (id INT PRIMARY KEY, name TEXT);
        CREATE VIEW public.active_users AS SELECT id FROM public.users;
        ",
    );
    assert!(
        output.contains("CREATE VIEW active_users AS"),
        "expected unqualified view name, got: {output}"
    );
    assert!(
        output.contains("FROM users"),
        "expected unqualified table reference in view query, got: {output}"
    );
    assert!(
        !output.contains("public."),
        "schema qualifiers should be removed for SQLite, got: {output}"
    );
}

#[test]
fn create_index_schema_qualified_target_is_unqualified() {
    let output = translate_single_statement("CREATE INDEX idx_users_name ON public.users(name);");
    assert!(
        output.contains("CREATE INDEX idx_users_name ON users (name)")
            || output.contains("CREATE INDEX idx_users_name ON users(name)"),
        "expected unqualified index target, got: {output}"
    );
    assert!(
        !output.contains("public.users"),
        "schema qualifier should be removed for SQLite, got: {output}"
    );
}

#[test]
fn create_index_schema_qualified_target_works_in_batch_translation() {
    let output = translated_sql(
        "
        CREATE TABLE public.users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_users_name ON public.users(name);
        ",
    );
    assert!(
        output.contains("CREATE INDEX idx_users_name ON users"),
        "expected schema-qualified index target to translate in batch mode, got: {output}"
    );
}

#[test]
fn create_trigger_schema_qualified_target_works_in_batch_translation() {
    let output = translated_sql(
        "
        CREATE TABLE public.docs (id INT PRIMARY KEY, name TEXT);
        CREATE FUNCTION docs_trigger_fn() RETURNS trigger AS $$
        BEGIN
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER docs_ai
        AFTER INSERT ON public.docs
        FOR EACH ROW
        EXECUTE FUNCTION docs_trigger_fn();
        ",
    );
    assert!(
        output.contains("CREATE TRIGGER docs_ai"),
        "expected schema-qualified trigger target to translate in batch mode, got: {output}"
    );
}

#[test]
fn non_public_schema_qualified_index_target_is_rejected() {
    let err = Pg2Sqlite::default()
        .sql(
            "
            CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
            CREATE INDEX idx_users_name ON my_custom_app.users(name);
            ",
        )
        .expect("sql should parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("non-public schema-qualified index target should be rejected");

    let message = err.to_string();
    assert!(
        message.contains("Only unqualified names and public.<table> names are supported"),
        "unexpected error: {message}"
    );
}

#[test]
fn non_public_schema_qualified_trigger_target_is_rejected() {
    let err = Pg2Sqlite::default()
        .sql(
            "
            CREATE TABLE docs (id INT PRIMARY KEY, name TEXT);
            CREATE FUNCTION docs_trigger_fn() RETURNS trigger AS $$
            BEGIN
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER docs_ai
            AFTER INSERT ON my_custom_app.docs
            FOR EACH ROW
            EXECUTE FUNCTION docs_trigger_fn();
            ",
        )
        .expect("sql should parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("non-public schema-qualified trigger target should be rejected");

    let message = err.to_string();
    assert!(
        message.contains("Only unqualified names and public.<table> names are supported"),
        "unexpected error: {message}"
    );
}

#[test]
fn three_part_index_target_is_rejected() {
    let err = Pg2Sqlite::default()
        .sql(
            "
            CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
            CREATE INDEX idx_users_name ON catalog.public.users(name);
            ",
        )
        .expect("sql should parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("three-part index target should be rejected");

    let message = err.to_string();
    assert!(
        message.to_lowercase().contains("object names with more than two parts"),
        "unexpected error: {message}"
    );
}

#[test]
fn drop_schema_qualified_object_names_are_unqualified() {
    let output = translated_sql(
        "
        DROP TABLE IF EXISTS public.users;
        DROP VIEW IF EXISTS public.active_users;
        DROP INDEX IF EXISTS public.idx_users_name;
        ",
    );
    assert!(
        !output.contains("public."),
        "DROP targets should be unqualified for SQLite, got: {output}"
    );
}

#[test]
fn translated_schema_qualified_sql_executes_in_sqlite() -> Result<(), Box<dyn std::error::Error>> {
    let translated = translate(
        "
        CREATE TABLE public.users (id INT PRIMARY KEY, name TEXT);
        CREATE VIEW public.active_users AS SELECT id, name FROM public.users;
        ",
    )?;

    let conn = Connection::open_in_memory()?;
    for stmt in translated {
        conn.execute_batch(&format!("{};", stmt))?;
    }

    Ok(())
}
