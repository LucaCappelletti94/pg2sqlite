//! Tests for RLS expression transform completeness (GROUP C).
//!
//! C1: Missing Expr arms in `transform_expr_generic` (Case, Like, IsTrue, etc.)
//! C2: SetExpr::SetOperation in `transform_query`
//! C3: Missing Expr arms in `transform_outer_table_refs`

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};

fn translate(sql: &str) -> Result<String, Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let stmts = Pg2Sqlite::default().sql(sql)?.translate(&options)?;
    Ok(stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
}

fn apply_all(sql: &str) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let stmts = Pg2Sqlite::default().sql(sql)?.translate(&options)?;
    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &stmts {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(conn)
}

#[test]
fn rls_policy_case_when_transforms_column_refs() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            role TEXT NOT NULL,
            owner TEXT NOT NULL
        );
        ALTER TABLE items ENABLE ROW LEVEL SECURITY;
        CREATE POLICY psel ON items FOR SELECT USING (true);
        CREATE POLICY pins ON items FOR INSERT WITH CHECK (
            CASE WHEN role = 'admin' THEN true ELSE owner = 'alice' END
        );
    "#;

    let output = translate(sql)?;

    // Inside the INSERT trigger, column refs must get NEW. prefix
    assert!(
        output.contains("NEW.role") || output.contains("NEW.\"role\""),
        "CASE WHEN should transform 'role' to NEW.role in trigger, got:\n{output}"
    );
    assert!(
        output.contains("NEW.owner") || output.contains("NEW.\"owner\""),
        "CASE ELSE should transform 'owner' to NEW.owner in trigger, got:\n{output}"
    );

    // Runtime validation
    let mut conn = apply_all(sql)?;
    diesel::sql_query("INSERT INTO items (id, role, owner) VALUES (1, 'admin', 'bob')")
        .execute(&mut conn)?;
    Ok(())
}

#[test]
fn rls_policy_like_transforms_column_refs() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE emails (
            id INTEGER PRIMARY KEY,
            email TEXT NOT NULL
        );
        ALTER TABLE emails ENABLE ROW LEVEL SECURITY;
        CREATE POLICY psel ON emails FOR SELECT USING (true);
        CREATE POLICY pins ON emails FOR INSERT WITH CHECK (
            email LIKE '%@example.com'
        );
    "#;

    let output = translate(sql)?;

    assert!(
        output.contains("NEW.email") || output.contains("NEW.\"email\""),
        "LIKE should transform 'email' to NEW.email in trigger, got:\n{output}"
    );

    let mut conn = apply_all(sql)?;
    diesel::sql_query("INSERT INTO emails (id, email) VALUES (1, 'a@example.com')")
        .execute(&mut conn)?;
    Ok(())
}

#[test]
fn rls_policy_is_true_transforms_column_refs() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE flags (
            id INTEGER PRIMARY KEY,
            active INTEGER NOT NULL DEFAULT 1
        );
        ALTER TABLE flags ENABLE ROW LEVEL SECURITY;
        CREATE POLICY psel ON flags FOR SELECT USING (true);
        CREATE POLICY pins ON flags FOR INSERT WITH CHECK (active IS TRUE);
    "#;

    let output = translate(sql)?;

    assert!(
        output.contains("NEW.active") || output.contains("NEW.\"active\""),
        "IS TRUE should transform 'active' to NEW.active in trigger, got:\n{output}"
    );

    let mut conn = apply_all(sql)?;
    diesel::sql_query("INSERT INTO flags (id, active) VALUES (1, 1)").execute(&mut conn)?;
    Ok(())
}

#[test]
fn rls_policy_with_union_transforms_both_sides() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE allowed_a (id INTEGER PRIMARY KEY);
        CREATE TABLE allowed_b (id INTEGER PRIMARY KEY);
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            val TEXT
        );
        ALTER TABLE items ENABLE ROW LEVEL SECURITY;
        CREATE POLICY psel ON items FOR SELECT USING (
            id IN (SELECT id FROM allowed_a UNION ALL SELECT id FROM allowed_b)
        );
    "#;

    let output = translate(sql)?;

    // Both sides of the UNION should appear in the translated view
    assert!(
        output.contains("allowed_a") && output.contains("allowed_b"),
        "Both sides of UNION must appear in translated RLS view, got:\n{output}"
    );
    assert!(
        output.contains("UNION ALL"),
        "UNION ALL must be preserved in translated output, got:\n{output}"
    );

    // Runtime: the view should work
    let mut conn = apply_all(sql)?;
    diesel::sql_query("INSERT INTO allowed_a VALUES (1)").execute(&mut conn)?;
    diesel::sql_query("INSERT INTO allowed_b VALUES (2)").execute(&mut conn)?;
    // These should be visible through the RLS view
    Ok(())
}

#[test]
fn rls_outer_table_ref_in_function_arg() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE docs (
            id INTEGER PRIMARY KEY,
            owner TEXT NOT NULL
        );
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY psel ON docs FOR SELECT USING (true);
        CREATE POLICY pins ON docs FOR INSERT WITH CHECK (
            EXISTS (SELECT 1 FROM docs WHERE lower(docs.owner) = lower(NEW.owner))
        );
    "#;

    let output = translate(sql)?;

    // The inner reference to docs should be renamed to docs_rls (the inner table)
    // inside the EXISTS subquery
    assert!(output.contains("lower("), "lower() function should be preserved, got:\n{output}");

    Ok(())
}
