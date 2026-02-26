//! TDD tests for column-level CHECK constraint translation (Section 1).

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// CHECK constraint survives translation and is enforced at runtime.
#[test]
fn test_check_constraint_is_translated() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE items (id SERIAL PRIMARY KEY, quantity INT CHECK (quantity > 0));";

    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let translated_sql = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");

    // The translated DDL must contain the CHECK clause
    assert!(
        translated_sql.contains("CHECK"),
        "CHECK constraint must appear in translated output, got: {translated_sql}"
    );

    // The CHECK must actually be enforced by SQLite at runtime
    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // Valid insert should succeed
    diesel::sql_query("INSERT INTO items (id, quantity) VALUES (1, 5)").execute(&mut conn)?;

    // Invalid insert must fail (quantity = -1 violates CHECK quantity > 0)
    let result =
        diesel::sql_query("INSERT INTO items (id, quantity) VALUES (2, -1)").execute(&mut conn);
    assert!(result.is_err(), "INSERT with quantity = -1 should violate CHECK constraint");

    Ok(())
}

/// CHECK constraint with a string expression is translated.
#[test]
fn test_check_constraint_string_length() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE products (id SERIAL PRIMARY KEY, code TEXT CHECK (length(code) >= 3));";

    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // Valid
    diesel::sql_query("INSERT INTO products (id, code) VALUES (1, 'ABC')").execute(&mut conn)?;

    // Too short — must fail
    let result =
        diesel::sql_query("INSERT INTO products (id, code) VALUES (2, 'AB')").execute(&mut conn);
    assert!(result.is_err(), "INSERT with code 'AB' (length 2) should violate CHECK");

    Ok(())
}
