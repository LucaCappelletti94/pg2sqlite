//! Tests for column option translation in
//! `src/impls/translator_impls/column_option.rs`.
//!
//! Covers: Default with UnaryOp, Nested, BinaryOp, Cast, generated columns
//! (ALWAYS), generated column (BY DEFAULT) error, and FK to RLS table.

#[path = "helpers/translate.rs"]
mod translate_helpers;
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation},
    traits::TranslationOptions,
};
use translate_helpers::translate_default as translate;

fn translate_with_options(sql: &str, options: &Pg2SqliteOptions) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(options)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn default_unary_op_negative() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, val INT DEFAULT -1);");
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
    assert!(output.contains("-1"), "Expected -1: {output}");
}

#[test]
fn default_nested_expression() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, val INT DEFAULT (42));");
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
    assert!(output.contains("42"), "Expected 42: {output}");
}

#[test]
fn default_binary_op() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, val INT DEFAULT 1 + 2);");
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
}

#[test]
fn default_cast_expression() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, val TEXT DEFAULT 'hello'::text);");
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
}

#[test]
fn default_literal_value() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, name TEXT DEFAULT 'unnamed');");
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
    assert!(output.contains("unnamed"), "Expected 'unnamed': {output}");
}

#[test]
fn default_identifier() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, val BOOLEAN DEFAULT true);");
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
}

#[test]
fn default_uuid_function() {
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text);
    let output = translate_with_options(
        "CREATE TABLE t (id UUID PRIMARY KEY DEFAULT gen_random_uuid(), name TEXT);",
        &options,
    );
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
    assert!(
        output.contains("DEFAULT (uuid())"),
        "Expected translated UUID function default expression: {output}"
    );
}

#[test]
fn default_schema_qualified_uuid_function() {
    let output = translate("CREATE TABLE t (id TEXT DEFAULT public.gen_random_uuid());");
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
    assert!(output.contains("uuid"), "Expected translated uuid function: {output}");
}

#[test]
fn default_uuid_generate_v4_function() {
    let result = Pg2Sqlite::default()
        .sql("CREATE TABLE t (id TEXT DEFAULT uuid_generate_v4());")
        .unwrap()
        .translate(&Pg2SqliteOptions::default());
    assert!(result.is_ok(), "uuid_generate_v4() default should be supported: {result:?}");
}

#[test]
fn generated_column_stored() {
    let output = translate(
        "CREATE TABLE t (id INT PRIMARY KEY, val INT, doubled INT GENERATED ALWAYS AS (val * 2) STORED);",
    );
    assert!(output.contains("GENERATED ALWAYS AS"), "Expected GENERATED ALWAYS AS: {output}");
}

#[test]
fn unique_constraint() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, email TEXT UNIQUE);");
    assert!(output.contains("UNIQUE"), "Expected UNIQUE: {output}");
}

#[test]
fn not_null_constraint() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, name TEXT NOT NULL);");
    assert!(output.contains("NOT NULL"), "Expected NOT NULL: {output}");
}

#[test]
fn check_constraint_translated() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, age INT CHECK (age >= 0));");
    assert!(output.contains("CHECK"), "CHECK should be translated: {output}");
    assert!(output.contains("age >= 0"), "CHECK condition should be preserved: {output}");
}

#[test]
fn fk_to_rls_table_gets_renamed() {
    let sql = r#"
        CREATE TABLE users (
            id UUID PRIMARY KEY,
            name TEXT NOT NULL
        );
        ALTER TABLE users ENABLE ROW LEVEL SECURITY;
        CREATE POLICY users_policy ON users FOR SELECT TO authenticated
            USING (id = current_setting('app.user_id')::uuid);
        CREATE POLICY users_insert ON users FOR INSERT TO authenticated
            WITH CHECK (id = current_setting('app.user_id')::uuid);
        CREATE POLICY users_update ON users FOR UPDATE TO authenticated
            USING (id = current_setting('app.user_id')::uuid)
            WITH CHECK (id = current_setting('app.user_id')::uuid);
        CREATE POLICY users_delete ON users FOR DELETE TO authenticated
            USING (id = current_setting('app.user_id')::uuid);
        CREATE TABLE orders (
            id UUID PRIMARY KEY,
            user_id UUID REFERENCES users(id)
        );
    "#;
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));
    let output = translate_with_options(sql, &options);
    assert!(
        output.contains("REFERENCES users_rls"),
        "FK should reference users_rls backing table: {output}"
    );
}

#[test]
fn fk_to_non_rls_table_unchanged() {
    let sql = r#"
        CREATE TABLE categories (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE products (id INT PRIMARY KEY, cat_id INT REFERENCES categories(id));
    "#;
    let output = translate(sql);
    assert!(
        output.contains("REFERENCES categories"),
        "FK should reference categories unchanged: {output}"
    );
}

/// PostgreSQL's `now()` function returns the current timestamp.
/// SQLite's equivalent is `datetime('now')`.
/// A column with `DEFAULT now()` must survive translation and work at runtime.
#[test]
fn default_now_translates_to_datetime_now() {
    let output =
        translate("CREATE TABLE now_test (id INTEGER PRIMARY KEY, created_at TEXT DEFAULT now());");
    assert!(
        output.contains("datetime") || output.contains("DATETIME"),
        "now() must become datetime('now'), got: {output}"
    );
    assert!(
        !output.to_lowercase().contains("now()"),
        "now() must be rewritten, not passed through, got: {output}"
    );

    // Verify the translated DDL works at runtime.
    use diesel::{Connection, RunQueryDsl, SqliteConnection};
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    diesel::sql_query(&output).execute(&mut conn).unwrap();
    // Insert without specifying created_at to exercise the DEFAULT.
    diesel::sql_query("INSERT INTO now_test (id) VALUES (1)").execute(&mut conn).unwrap();

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        created_at: Option<String>,
    }
    let rows = diesel::sql_query("SELECT created_at FROM now_test").load::<Row>(&mut conn).unwrap();
    assert_eq!(rows.len(), 1, "Should have one row");
    assert!(rows[0].created_at.is_some(), "DEFAULT datetime('now') should set a timestamp");
}

/// SQLite's `DEFAULT` clause takes a literal, a signed number, a bare keyword,
/// or a *parenthesized* expression: `DEFAULT 1 + 2` and
/// `DEFAULT CAST(x AS TEXT)` are both syntax errors. The older per-shape
/// translation emitted them bare, so every `DEFAULT` shape is checked here by
/// executing the emitted DDL rather than by grepping for the keyword.
///
/// The DDL is executed as generated text through `rusqlite` because the string
/// the translator produced is exactly what is under test; a typed diesel schema
/// would describe a table this test is trying to discover the shape of.
#[test]
fn every_default_shape_produces_runnable_ddl() {
    let ddl = translate(
        "CREATE TABLE defaults (
            id INT PRIMARY KEY,
            literal TEXT DEFAULT 'a',
            signed INT DEFAULT -1,
            parenthesized INT DEFAULT (42),
            arithmetic INT DEFAULT 1 + 2,
            casted TEXT DEFAULT 'hello'::text,
            keyword TEXT DEFAULT CURRENT_TIMESTAMP,
            call TEXT DEFAULT now(),
            boolean BOOLEAN DEFAULT true
         );",
    );

    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    conn.execute_batch(&format!("{ddl};"))
        .unwrap_or_else(|e| panic!("emitted DDL is not runnable: {e}\n{ddl}"));

    // A bare literal or signed number must stay bare: wrapping is harmless but
    // the assertion pins which forms need parentheses and which do not.
    assert!(ddl.contains("literal TEXT DEFAULT 'a'"), "{ddl}");
    assert!(ddl.contains("signed INTEGER DEFAULT -1"), "{ddl}");
    assert!(ddl.contains("arithmetic INTEGER DEFAULT (1 + 2)"), "{ddl}");
    assert!(ddl.contains("casted TEXT DEFAULT (CAST('hello' AS TEXT))"), "{ddl}");
    assert!(ddl.contains("keyword TEXT DEFAULT CURRENT_TIMESTAMP"), "{ddl}");
    assert!(ddl.contains("call TEXT DEFAULT (datetime('now'))"), "{ddl}");
}

#[test]
fn constraint_characteristics_translation_is_rejected() {
    use pg2sqlite::{errors::Error, options::Pg2SqliteOptions as Opts, prelude::Translator};
    use sql_traits::structs::ParserDB;
    use sqlparser::ast::{ConstraintCharacteristics, DeferrableInitial};

    let schema =
        ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build");
    let options = Opts::default();
    let characteristics = ConstraintCharacteristics {
        deferrable: Some(true),
        initially: Some(DeferrableInitial::Deferred),
        enforced: Some(false),
    };

    let error = characteristics
        .translate(&schema, &options)
        .expect_err("constraint characteristics are unsupported in SQLite translation");
    assert!(matches!(
        error,
        Error::UnsupportedSQLiteFeature(message)
            if message.contains("Unsupported constraint characteristic")
    ));
}

#[test]
fn referential_action_translation_passthrough_covers_all_variants() {
    use pg2sqlite::{options::Pg2SqliteOptions as Opts, prelude::Translator};
    use sql_traits::structs::ParserDB;
    use sqlparser::ast::ReferentialAction;

    let schema =
        ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build");
    let options = Opts::default();

    for action in [
        ReferentialAction::NoAction,
        ReferentialAction::Restrict,
        ReferentialAction::SetNull,
        ReferentialAction::SetDefault,
        ReferentialAction::Cascade,
    ] {
        let translated = action
            .translate(&schema, &options)
            .expect("referential actions should translate as-is");
        assert_eq!(translated, action);
    }
}

#[test]
fn collate_option_silently_dropped() {
    let output = translate(r#"CREATE TABLE t (id INT PRIMARY KEY, col TEXT COLLATE "de_DE");"#);
    assert!(!output.contains("COLLATE"), "COLLATE should be dropped, got: {output}");
}

#[test]
fn character_set_option_silently_dropped() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, col TEXT CHARACTER SET utf8mb4);");
    assert!(!output.contains("CHARACTER SET"), "CHARACTER SET should be dropped, got: {output}");
}

#[test]
fn comment_option_silently_dropped() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, col INT COMMENT 'desc');");
    assert!(!output.contains("COMMENT"), "COMMENT should be dropped, got: {output}");
}
