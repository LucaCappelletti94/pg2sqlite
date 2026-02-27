//! Tests for CREATE TABLE ... AS SELECT subquery translation (GROUP B).
//!
//! Verifies that PG functions inside the AS SELECT subquery are translated
//! (e.g. `now()` → `datetime('now')`), not cloned verbatim.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> Result<String, Box<dyn std::error::Error>> {
    let stmts = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    Ok(stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
}

#[test]
fn create_table_as_select_translates_functions() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE snapshots AS SELECT id, now() AS created_at FROM source;";
    let output = translate(sql)?;

    assert!(
        output.contains("datetime('now')"),
        "now() should be translated to datetime('now') in AS SELECT, got:\n{output}"
    );
    assert!(
        !output.contains("now()"),
        "raw now() should not leak through to SQLite, got:\n{output}"
    );

    Ok(())
}

#[test]
fn create_table_as_select_basic() -> Result<(), Box<dyn std::error::Error>> {
    // Simple CTAS without PG-specific functions should translate cleanly
    let sql = "CREATE TABLE archive AS SELECT id, name FROM users WHERE active = true;";
    let output = translate(sql)?;

    assert!(output.contains("CREATE TABLE"), "Should contain CREATE TABLE");
    assert!(output.contains("SELECT"), "Should contain SELECT subquery");

    // Verify it executes in SQLite (after creating the source table)
    let mut conn = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("CREATE TABLE users (id INTEGER, name TEXT, active INTEGER) STRICT")
        .execute(&mut conn)?;
    diesel::sql_query("INSERT INTO users VALUES (1, 'Alice', 1)").execute(&mut conn)?;

    // The translated CTAS should run without error
    diesel::sql_query(output).execute(&mut conn)?;
    Ok(())
}
