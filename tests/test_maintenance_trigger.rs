//! Test for maintenance trigger translation.

use diesel::{Connection, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};

// Schema definitions for test tables
diesel::table! {
    /// Brands table for testing maintenance triggers.
    brands (id) {
        /// Brand ID.
        id -> Integer,
        /// Brand name.
        name -> Text,
        /// Timestamp of last edit.
        edited_at -> Nullable<Text>,
    }
}

/// A brand record with auto-updated edit timestamp.
#[derive(Queryable, Selectable)]
#[diesel(table_name = brands)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
#[allow(dead_code)]
struct Brand {
    /// Brand ID.
    id: i32,
    /// Brand name.
    name: String,
    /// Timestamp of last edit.
    edited_at: Option<String>,
}

/// Insertable brand record.
#[derive(Insertable)]
#[diesel(table_name = brands)]
struct NewBrand {
    /// Brand ID.
    id: i32,
    /// Brand name.
    name: String,
    /// Timestamp of last edit.
    edited_at: Option<String>,
}

#[test]
fn test_maintenance_trigger() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
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
    diesel::insert_into(brands::table)
        .values(&NewBrand {
            id: 1,
            name: "Adidas".to_string(),
            edited_at: Some("2020-01-01".to_string()),
        })
        .execute(&mut connection)?;

    // Update data (trigger should fire)
    diesel::update(brands::table.filter(brands::id.eq(1)))
        .set(brands::name.eq("Nike"))
        .execute(&mut connection)?;

    // Verify by updating if condition is met
    let count = diesel::update(
        brands::table.filter(brands::id.eq(1).and(brands::edited_at.ne("2020-01-01"))),
    )
    .set(brands::name.eq("Verified"))
    .execute(&mut connection)?;

    assert_eq!(count, 1, "Expected 1 row updated, meaning edited_at changed from initial value");

    Ok(())
}

#[test]
fn test_maintenance_trigger_on_rls_table() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
CREATE TABLE brands (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    edited_at TEXT
);

ALTER TABLE brands ENABLE ROW LEVEL SECURITY;
CREATE POLICY brands_select_all ON brands FOR SELECT USING (true);
CREATE POLICY brands_insert_all ON brands FOR INSERT WITH CHECK (true);
CREATE POLICY brands_update_all ON brands FOR UPDATE USING (true) WITH CHECK (true);

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

    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let translator = Pg2Sqlite::default().sql(sql)?;
    let translated = translator.translate(&options)?;

    let mut connection = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;

    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }

    diesel::sql_query(
        "INSERT INTO brands (id, name, edited_at) VALUES (1, 'Adidas', '2020-01-01')",
    )
    .execute(&mut connection)?;

    diesel::sql_query("UPDATE brands SET name = 'Nike' WHERE id = 1").execute(&mut connection)?;

    let updated =
        brands::table.filter(brands::id.eq(1)).select(Brand::as_select()).first(&mut connection)?;
    assert_eq!(updated.name, "Nike");

    Ok(())
}
