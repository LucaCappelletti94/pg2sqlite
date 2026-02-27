//! Tests for RLS policy expression rewriting and trigger translation.
//!
//! Covers function argument rewriting in WITH CHECK, non-session cast
//! preservation, and quoted-table update trigger execution.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};
use sqlparser::ast::Statement;

fn translate(sql: &str) -> Result<Vec<Statement>, Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    Ok(Pg2Sqlite::default().sql(sql)?.translate(&options)?)
}

fn apply_all(stmts: &[Statement]) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in stmts {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(conn)
}

/// Regression: function arguments inside WITH CHECK policies must rewrite
/// column refs (e.g. lower(owner) -> lower(NEW.owner)).
#[test]
fn rls_with_check_function_arg_rewrite_executes_in_sqlite() -> Result<(), Box<dyn std::error::Error>>
{
    let sql = r#"
        CREATE TABLE docs (
            id INTEGER PRIMARY KEY,
            owner TEXT NOT NULL
        );
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_select ON docs FOR SELECT USING (true);
        CREATE POLICY docs_insert ON docs FOR INSERT WITH CHECK (lower(owner) = 'alice');
    "#;

    let translated = translate(sql)?;
    let mut conn = apply_all(&translated)?;

    // Should execute through INSTEAD OF INSERT trigger.
    // Buggy code emits lower(owner), which crashes at runtime with:
    // "no such column: owner".
    diesel::sql_query("INSERT INTO docs (id, owner) VALUES (1, 'alice')").execute(&mut conn)?;
    Ok(())
}

/// Regression: non-session CAST nodes in policies must be preserved.
#[test]
fn rls_policy_preserves_non_session_casts() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE docs (
            id INTEGER PRIMARY KEY,
            owner TEXT NOT NULL
        );
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_select ON docs FOR SELECT USING (true);
        CREATE POLICY docs_insert ON docs FOR INSERT WITH CHECK (
            CAST(owner AS TEXT) = CAST('alice' AS TEXT)
        );
    "#;

    let translated = translate(sql)?;
    let all_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        all_sql.contains("CAST(NEW.owner AS TEXT)"),
        "left-side CAST should be preserved in generated trigger SQL, got:\n{all_sql}"
    );
    assert!(
        all_sql.contains("CAST('alice' AS TEXT)"),
        "right-side CAST should be preserved in generated trigger SQL, got:\n{all_sql}"
    );

    Ok(())
}

/// Regression: quoted table names must remain quoted in UPDATE trigger DML.
#[test]
fn rls_quoted_table_update_trigger_executes_in_sqlite() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE "Order Items"(
            "doc id" INTEGER PRIMARY KEY,
            "owner id" INTEGER,
            body TEXT
        );
        ALTER TABLE "Order Items" ENABLE ROW LEVEL SECURITY;
        CREATE POLICY psel ON "Order Items" FOR SELECT USING ("owner id" > 0);
        CREATE POLICY pupd ON "Order Items" FOR UPDATE USING ("owner id" > 0) WITH CHECK ("owner id" > 0);
    "#;

    let translated = translate(sql)?;

    // Should apply cleanly in SQLite.
    // Buggy code generates `UPDATE Order Items_rls ...`, causing parse error.
    let _conn = apply_all(&translated)?;
    Ok(())
}
