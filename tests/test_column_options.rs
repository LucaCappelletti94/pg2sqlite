//! Tests for column option translation in
//! `src/impls/translator_impls/column_option.rs`.
//!
//! Covers: Default with UnaryOp, Nested, BinaryOp, Cast, generated columns
//! (ALWAYS), generated column (BY DEFAULT) error, and FK to RLS table.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation},
    traits::TranslationOptions,
};

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

// ==================== Default with UnaryOp ====================

#[test]
fn default_unary_op_negative() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, val INT DEFAULT -1);");
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
    assert!(output.contains("-1"), "Expected -1: {output}");
}

// ==================== Default with Nested ====================

#[test]
fn default_nested_expression() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, val INT DEFAULT (42));");
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
    assert!(output.contains("42"), "Expected 42: {output}");
}

// ==================== Default with BinaryOp ====================

#[test]
fn default_binary_op() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, val INT DEFAULT 1 + 2);");
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
}

// ==================== Default with Cast ====================

#[test]
fn default_cast_expression() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, val TEXT DEFAULT 'hello'::text);");
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
}

// ==================== Default with Value ====================

#[test]
fn default_literal_value() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, name TEXT DEFAULT 'unnamed');");
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
    assert!(output.contains("unnamed"), "Expected 'unnamed': {output}");
}

// ==================== Default with Identifier ====================

#[test]
fn default_identifier() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, val BOOLEAN DEFAULT true);");
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
}

// ==================== Default with UUID function ====================

#[test]
fn default_uuid_function() {
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text);
    let output = translate_with_options(
        "CREATE TABLE t (id UUID PRIMARY KEY DEFAULT gen_random_uuid(), name TEXT);",
        &options,
    );
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
    assert!(output.contains("uuid"), "Expected uuid function: {output}");
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

// ==================== Generated column (ALWAYS) ====================

#[test]
fn generated_column_stored() {
    let output = translate(
        "CREATE TABLE t (id INT PRIMARY KEY, val INT, doubled INT GENERATED ALWAYS AS (val * 2) STORED);",
    );
    assert!(output.contains("GENERATED ALWAYS AS"), "Expected GENERATED ALWAYS AS: {output}");
}

// ==================== Unique constraint ====================

#[test]
fn unique_constraint() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, email TEXT UNIQUE);");
    assert!(output.contains("UNIQUE"), "Expected UNIQUE: {output}");
}

// ==================== NOT NULL ====================

#[test]
fn not_null_constraint() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, name TEXT NOT NULL);");
    assert!(output.contains("NOT NULL"), "Expected NOT NULL: {output}");
}

// ==================== CHECK constraint (should be stripped)
// ====================

#[test]
fn check_constraint_stripped() {
    let output = translate("CREATE TABLE t (id INT PRIMARY KEY, age INT CHECK (age >= 0));");
    assert!(!output.contains("CHECK"), "CHECK should be stripped: {output}");
}

// ==================== FK to RLS table ====================

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

// ==================== FK to non-RLS table stays unchanged ====================

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
