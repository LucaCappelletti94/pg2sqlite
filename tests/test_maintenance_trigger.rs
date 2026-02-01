//! Test for maintenance trigger translation.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[test]
fn test_maintenance_trigger() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r"
CREATE TABLE brands (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    edited_at TEXT
);

CREATE OR REPLACE FUNCTION update_brands_edited_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.edited_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_brands_edited_at
BEFORE UPDATE ON brands
FOR EACH ROW EXECUTE FUNCTION update_brands_edited_at();
";

    let translator = Pg2Sqlite::default().sql(sql)?;
    let translated = translator.translate(&Pg2SqliteOptions::default())?;

    for stmt in &translated {
        println!("{stmt}");
    }

    let mut connection = SqliteConnection::establish(":memory:")?;

    // Setup SQLite environment
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;

    // Run translations
    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }

    // Insert data
    diesel::sql_query(
        "INSERT INTO brands (id, name, edited_at) VALUES (1, 'Adidas', '2020-01-01')",
    )
    .execute(&mut connection)?;

    // Update data (trigger should fire)
    diesel::sql_query("UPDATE brands SET name = 'Nike' WHERE id = 1").execute(&mut connection)?;

    // Verify by updating if condition is met
    let count = diesel::sql_query(
        "UPDATE brands SET name = 'Verified' WHERE id = 1 AND edited_at != '2020-01-01'",
    )
    .execute(&mut connection)?;

    assert_eq!(count, 1, "Expected 1 row updated, meaning edited_at changed from initial value");

    Ok(())
}
