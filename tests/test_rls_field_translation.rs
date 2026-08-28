//! Tests for GROUP I: RLS field-level clones.
//!
//! I1a: FunctionArgumentList.clauses not transformed in RLS
//! I1b: WindowFrame bounds not transformed in RLS
//! I1c: Function parameters not transformed in RLS

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

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
fn rls_window_frame_bound_appears_in_output() -> Result<(), Box<dyn std::error::Error>> {
    // Window function with ROWS BETWEEN in RLS subquery — the bound expressions
    // should be transformed if they reference columns
    let sql = r#"
        CREATE TABLE metrics (
            id INTEGER PRIMARY KEY,
            val REAL NOT NULL
        );
        ALTER TABLE metrics ENABLE ROW LEVEL SECURITY;
        CREATE POLICY psel ON metrics FOR SELECT USING (true);
        CREATE POLICY pins ON metrics FOR INSERT WITH CHECK (
            val <= (
                SELECT AVG(m.val) OVER (ORDER BY m.id ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING)
                FROM metrics m
                LIMIT 1
            )
        );
    "#;

    let output = translate(sql)?;
    let lower = output.to_lowercase();

    // The window frame with ROWS BETWEEN should survive translation
    assert!(
        lower.contains("1 preceding") || lower.contains("rows between"),
        "Window frame bounds should appear in output: {output}"
    );
    // Execute the emitted DDL to prove SQLite accepts the translated statements.
    // Translated SQL is dynamic and cannot be expressed with diesel's typed DSL.
    apply_all(sql)?;
    Ok(())
}

#[test]
fn rls_function_args_in_policy_transformed() -> Result<(), Box<dyn std::error::Error>> {
    // A function with column ref arg in INSERT policy — must get NEW. prefix
    let sql = r#"
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            email TEXT NOT NULL
        );
        ALTER TABLE users ENABLE ROW LEVEL SECURITY;
        CREATE POLICY usel ON users FOR SELECT USING (true);
        CREATE POLICY uins ON users FOR INSERT WITH CHECK (
            LENGTH(email) > 0
        );
    "#;

    let output = translate(sql)?;

    // Verify column ref 'email' becomes NEW.email in INSERT trigger
    assert!(
        output.contains("NEW.email") || output.contains("NEW.\"email\""),
        "Column ref 'email' should become NEW.email in INSERT trigger: {output}"
    );
    // Execute the emitted DDL to prove SQLite accepts the translated statements.
    // Translated SQL is dynamic and cannot be expressed with diesel's typed DSL.
    apply_all(sql)?;
    Ok(())
}

#[test]
fn rls_function_in_policy_executes_in_sqlite() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r#"
        CREATE TABLE products (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            price REAL NOT NULL
        );
        ALTER TABLE products ENABLE ROW LEVEL SECURITY;
        CREATE POLICY psel ON products FOR SELECT USING (true);
        CREATE POLICY pcheck ON products FOR INSERT
            WITH CHECK (price > 0 AND LENGTH(name) > 0);
    "#;

    let conn = &mut apply_all(sql)?;
    // Insert valid data through the view
    diesel::sql_query("INSERT INTO products (id, name, price) VALUES (1, 'Widget', 9.99)")
        .execute(conn)?;
    Ok(())
}
