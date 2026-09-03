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

/// Translates `pg_sql` and executes every emitted statement against an
/// in-memory SQLite connection to verify the output is valid SQLite.
fn execute_as_sqlite(pg_sql: &str) {
    let stmts = translate(pg_sql).expect("translation should succeed");
    let conn = Connection::open_in_memory().expect("open in-memory SQLite");
    for stmt in &stmts {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("translated SQL failed in SQLite: {e}\nSQL: {stmt}"));
    }
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
    execute_as_sqlite("CREATE TABLE public.users (id INT PRIMARY KEY, name TEXT);");
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
    execute_as_sqlite(
        "
        CREATE TABLE public.users (id INT PRIMARY KEY, name TEXT);
        CREATE VIEW public.active_users AS SELECT id FROM public.users;
        ",
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
    // This test uses translate_single_statement (empty schema context), so we
    // create the prerequisite table inline before running the translated index.
    {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);").unwrap();
        conn.execute_batch(&format!("{output};")).unwrap();
    }
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
    execute_as_sqlite(
        "
        CREATE TABLE public.users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_users_name ON public.users(name);
        ",
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
    execute_as_sqlite(
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
        message.contains("Unsupported schema-qualified object name")
            && message.contains("does not resolve"),
        "unexpected error: {message}"
    );
}

#[test]
fn non_public_schema_qualified_index_target_is_unqualified_when_schema_resolves() {
    let output = translated_sql(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_users_name ON my_custom_app.users(name);
        ",
    );
    assert!(
        output.contains("CREATE INDEX idx_users_name ON users"),
        "expected schema-qualified index target to translate when schema resolves, got: {output}"
    );
    assert!(
        !output.contains("my_custom_app."),
        "schema qualifier should be removed for SQLite, got: {output}"
    );
    execute_as_sqlite(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_users_name ON my_custom_app.users(name);
        ",
    );
}

#[test]
fn non_public_schema_qualified_create_table_is_unqualified_when_schema_resolves() {
    let output = translated_sql(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);
        ",
    );
    assert!(
        output.contains("CREATE TABLE users"),
        "expected unqualified CREATE TABLE output, got: {output}"
    );
    assert!(
        !output.contains("my_custom_app."),
        "schema qualifier should be removed for SQLite, got: {output}"
    );
    execute_as_sqlite(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);
        ",
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
        message.contains("Unsupported schema-qualified object name")
            && message.contains("does not resolve"),
        "unexpected error: {message}"
    );
}

#[test]
fn non_public_schema_qualified_trigger_target_is_unqualified_when_schema_resolves() {
    let output = translated_sql(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.docs (id INT PRIMARY KEY, name TEXT);
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
    );
    assert!(
        output.contains("CREATE TRIGGER docs_ai"),
        "expected trigger translation to succeed, got: {output}"
    );
    assert!(
        !output.contains("my_custom_app."),
        "schema qualifier should be removed for SQLite, got: {output}"
    );
    execute_as_sqlite(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.docs (id INT PRIMARY KEY, name TEXT);
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
    );
}

#[test]
fn non_public_schema_qualified_delete_target_is_rejected() {
    let err = Pg2Sqlite::default()
        .sql(
            "
            CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
            DELETE FROM my_custom_app.users WHERE id = 1;
            ",
        )
        .expect("sql should parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("non-public schema-qualified delete target should be rejected");

    let message = err.to_string();
    assert!(
        message.contains("id cannot be resolved to a declared column")
            && message.contains("without an inspectable definition"),
        "unexpected error: {message}"
    );
}

#[test]
fn non_public_schema_qualified_delete_target_is_unqualified_when_schema_resolves() {
    let output = translated_sql(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);
        DELETE FROM my_custom_app.users WHERE id = 1;
        ",
    );
    assert!(
        output.contains("DELETE FROM users WHERE id = 1"),
        "expected DELETE target to be unqualified, got: {output}"
    );
    assert!(
        !output.contains("my_custom_app."),
        "schema qualifier should be removed for SQLite, got: {output}"
    );
    execute_as_sqlite(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);
        DELETE FROM my_custom_app.users WHERE id = 1;
        ",
    );
}

#[test]
fn schema_qualified_insert_target_is_unqualified() {
    let output = translated_sql(
        "
        CREATE TABLE public.users (id INT PRIMARY KEY, name TEXT);
        INSERT INTO public.users (id, name) VALUES (1, 'a');
        ",
    );
    assert!(
        output.contains("INSERT INTO users"),
        "expected INSERT target to be unqualified, got: {output}"
    );
    execute_as_sqlite(
        "
        CREATE TABLE public.users (id INT PRIMARY KEY, name TEXT);
        INSERT INTO public.users (id, name) VALUES (1, 'a');
        ",
    );
}

#[test]
fn non_public_schema_qualified_insert_target_is_unqualified_when_schema_resolves() {
    let sql = "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);
        INSERT INTO my_custom_app.users (id, name) VALUES (1, 'a');
    ";
    let output = translated_sql(sql);
    assert!(
        !output.contains("my_custom_app."),
        "schema qualifier should be removed for SQLite, got: {output}"
    );
    execute_as_sqlite(sql);
}

#[test]
fn non_public_schema_qualified_select_from_target_is_rejected() {
    let err = Pg2Sqlite::default()
        .sql(
            "
            CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
            CREATE VIEW active_users AS SELECT id FROM my_custom_app.users;
            ",
        )
        .expect("sql should parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("non-public schema-qualified select FROM target should be rejected");

    let message = err.to_string();
    assert!(
        message.contains("Unsupported schema-qualified object name")
            && message.contains("does not resolve"),
        "unexpected error: {message}"
    );
}

#[test]
fn non_public_schema_qualified_join_target_is_unqualified_when_schema_resolves() {
    let output = translated_sql(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE teams (id INT PRIMARY KEY, owner_id INT);
        CREATE VIEW team_owners AS
        SELECT u.id
        FROM my_custom_app.users u
        JOIN teams t ON t.owner_id = u.id;
        ",
    );
    assert!(
        output.contains("CREATE VIEW team_owners AS"),
        "expected view translation to succeed, got: {output}"
    );
    assert!(
        output.contains("FROM users"),
        "expected schema-qualified FROM target to be unqualified, got: {output}"
    );
    assert!(output.contains("JOIN teams"), "expected join source to be unqualified, got: {output}");
    assert!(
        !output.contains("my_custom_app."),
        "schema qualifier should be removed for SQLite, got: {output}"
    );
    execute_as_sqlite(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE teams (id INT PRIMARY KEY, owner_id INT);
        CREATE VIEW team_owners AS
        SELECT u.id
        FROM my_custom_app.users u
        JOIN teams t ON t.owner_id = u.id;
        ",
    );
}

#[test]
fn non_public_schema_qualified_join_target_is_rejected() {
    let err = Pg2Sqlite::default()
        .sql(
            "
            CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
            CREATE TABLE teams (id INT PRIMARY KEY, owner_id INT);
            CREATE VIEW team_owners AS
            SELECT u.id
            FROM my_custom_app.users u
            JOIN teams t ON t.owner_id = u.id;
            ",
        )
        .expect("sql should parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("non-public schema-qualified join target should be rejected");

    let message = err.to_string();
    assert!(
        message.contains("u.id cannot be resolved to a declared column")
            && message.contains("without an inspectable definition"),
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
    execute_as_sqlite(
        "
        DROP TABLE IF EXISTS public.users;
        DROP VIEW IF EXISTS public.active_users;
        DROP INDEX IF EXISTS public.idx_users_name;
        ",
    );
}

#[test]
fn drop_non_public_schema_qualified_object_names_error_when_schema_unresolved() {
    let err = Pg2Sqlite::default()
        .sql("DROP TABLE IF EXISTS my_custom_app.users;")
        .expect("sql should parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("unresolved non-public DROP target should error");
    let message = err.to_string();
    assert!(
        message.contains("Unsupported schema-qualified object name")
            && message.contains("does not resolve"),
        "unexpected error: {message}"
    );
}

#[test]
fn drop_non_public_schema_qualified_object_names_are_unqualified_when_schema_resolves() {
    let output = translated_sql(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);
        DROP TABLE IF EXISTS my_custom_app.users;
        ",
    );
    assert!(
        output.contains("DROP TABLE IF EXISTS users"),
        "expected DROP target to be unqualified when schema resolves, got: {output}"
    );
    assert!(
        !output.contains("my_custom_app."),
        "schema qualifier should be removed for SQLite, got: {output}"
    );
    execute_as_sqlite(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);
        DROP TABLE IF EXISTS my_custom_app.users;
        ",
    );
}

#[test]
fn create_view_non_public_schema_name_is_unqualified_when_schema_resolves() {
    let output = translated_sql(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);
        CREATE VIEW my_custom_app.active_users AS SELECT id FROM my_custom_app.users;
        ",
    );
    assert!(
        output.contains("CREATE VIEW active_users AS SELECT id FROM users"),
        "expected schema-qualified view + source to be unqualified in output, got: {output}"
    );
    execute_as_sqlite(
        "
        CREATE SCHEMA IF NOT EXISTS my_custom_app;
        CREATE TABLE my_custom_app.users (id INT PRIMARY KEY, name TEXT);
        CREATE VIEW my_custom_app.active_users AS SELECT id FROM my_custom_app.users;
        ",
    );
}

#[test]
fn create_view_non_public_schema_name_errors_when_schema_unresolved() {
    let err = Pg2Sqlite::default()
        .sql("CREATE VIEW my_custom_app.active_users AS SELECT 1;")
        .expect("sql should parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("unresolved non-public view schema should error");
    let message = err.to_string();
    assert!(
        message.contains("Schema `my_custom_app` not found")
            && message.contains("View `active_users`"),
        "unexpected error: {message}"
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
