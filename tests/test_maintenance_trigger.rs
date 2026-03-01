//! Test for maintenance trigger translation.

use diesel::{Connection, QueryableByName, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, Translator},
    traits::TranslationOptions,
};
use sql_traits::structs::ParserDB;
use sqlparser::{ast::Statement, dialect::PostgreSqlDialect, parser::Parser};

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

#[derive(QueryableByName)]
struct TriggerDefinition {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    sql: String,
}

fn translate_with_direct_create_trigger_path(
    sql: &str,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, Box<dyn std::error::Error>> {
    let pg_statements = Parser::parse_sql(&PostgreSqlDialect {}, sql)?;
    let schema = ParserDB::from_statements(pg_statements.clone(), "test".to_string())?;
    let mut translated = Vec::new();

    for statement in pg_statements {
        match statement {
            Statement::CreateTrigger(create_trigger) => {
                for (maybe_drop, translated_trigger) in
                    create_trigger.translate(&schema, options)?
                {
                    if let Some(drop_trigger) = maybe_drop {
                        translated.push(Statement::DropTrigger(drop_trigger));
                    }
                    translated.push(Statement::CreateTrigger(translated_trigger));
                }
            }
            other => translated.extend(other.translate(&schema, options)?),
        }
    }

    Ok(translated)
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
fn test_maintenance_trigger_with_recursive_triggers_enabled()
-> Result<(), Box<dyn std::error::Error>> {
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

    let trigger_sql = translated
        .iter()
        .map(ToString::to_string)
        .find(|sql| sql.contains("CREATE TRIGGER trigger_update_brands_edited_at"))
        .expect("translated trigger statement should exist");
    assert!(
        trigger_sql.contains("UPDATE OF id, name ON brands"),
        "maintenance trigger should exclude maintenance columns from UPDATE event: {trigger_sql}"
    );

    let mut connection = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;

    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }

    diesel::insert_into(brands::table)
        .values(&NewBrand {
            id: 1,
            name: "Adidas".to_string(),
            edited_at: Some("2020-01-01".to_string()),
        })
        .execute(&mut connection)?;

    diesel::update(brands::table.filter(brands::id.eq(1)))
        .set(brands::name.eq("Nike"))
        .execute(&mut connection)?;

    let updated =
        brands::table.filter(brands::id.eq(1)).select(Brand::as_select()).first(&mut connection)?;
    assert_eq!(updated.name, "Nike");

    Ok(())
}

#[test]
fn test_maintenance_trigger_before_insert() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
CREATE TABLE brands (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    edited_at TEXT
);

CREATE OR REPLACE FUNCTION set_brands_edited_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.edited_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_insert_brands_edited_at
BEFORE INSERT ON brands
FOR EACH ROW EXECUTE FUNCTION set_brands_edited_at();
";

    let translator = Pg2Sqlite::default().sql(sql)?;
    let translated = translator.translate(&Pg2SqliteOptions::default())?;

    let trigger_sql = translated
        .iter()
        .map(ToString::to_string)
        .find(|sql| sql.contains("CREATE TRIGGER trigger_insert_brands_edited_at"))
        .expect("translated trigger statement should exist");
    assert!(
        trigger_sql.contains("AFTER INSERT ON brands"),
        "maintenance insert trigger should be translated to AFTER INSERT: {trigger_sql}"
    );

    let mut connection = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;

    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }

    diesel::insert_into(brands::table)
        .values(&NewBrand { id: 1, name: "Adidas".to_string(), edited_at: None })
        .execute(&mut connection)?;

    let inserted =
        brands::table.filter(brands::id.eq(1)).select(Brand::as_select()).first(&mut connection)?;
    assert!(
        inserted.edited_at.is_some(),
        "edited_at should be populated by translated maintenance insert trigger"
    );

    Ok(())
}

#[test]
fn test_maintenance_trigger_before_insert_or_update_splits_trigger()
-> Result<(), Box<dyn std::error::Error>> {
    let sql = "
CREATE TABLE brands (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    edited_at TEXT
);

CREATE OR REPLACE FUNCTION set_brands_edited_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.edited_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_upsert_brands_edited_at
BEFORE INSERT OR UPDATE ON brands
FOR EACH ROW EXECUTE FUNCTION set_brands_edited_at();
";

    let translator = Pg2Sqlite::default().sql(sql)?;
    let translated = translator.translate(&Pg2SqliteOptions::default())?;
    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>();

    let update_trigger_sql = translated_sql
        .iter()
        .find(|stmt| stmt.contains("CREATE TRIGGER trigger_upsert_brands_edited_at "))
        .expect("translated BEFORE UPDATE trigger should exist");
    assert!(
        update_trigger_sql.contains("BEFORE UPDATE OF id, name ON brands"),
        "maintenance update branch should remain BEFORE UPDATE: {update_trigger_sql}"
    );

    let insert_trigger_sql = translated_sql
        .iter()
        .find(|stmt| {
            stmt.contains("CREATE TRIGGER trigger_upsert_brands_edited_at_pg2sqlite_insert")
        })
        .expect("translated AFTER INSERT trigger should exist");
    assert!(
        insert_trigger_sql.contains("AFTER INSERT ON brands"),
        "maintenance insert branch should be translated to AFTER INSERT: {insert_trigger_sql}"
    );

    let mut connection = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;

    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }

    diesel::insert_into(brands::table)
        .values(&NewBrand { id: 1, name: "Adidas".to_string(), edited_at: None })
        .execute(&mut connection)?;

    let inserted =
        brands::table.filter(brands::id.eq(1)).select(Brand::as_select()).first(&mut connection)?;
    assert!(
        inserted.edited_at.is_some(),
        "edited_at should be populated after INSERT by the split insert trigger"
    );

    diesel::update(brands::table.filter(brands::id.eq(1)))
        .set(brands::name.eq("Nike"))
        .execute(&mut connection)?;

    diesel::update(brands::table.filter(brands::id.eq(1)))
        .set(brands::edited_at.eq("manual"))
        .execute(&mut connection)?;

    let updated =
        brands::table.filter(brands::id.eq(1)).select(Brand::as_select()).first(&mut connection)?;
    assert_eq!(
        updated.edited_at.as_deref(),
        Some("manual"),
        "UPDATE branch should exclude maintenance column to avoid self-recursion"
    );

    Ok(())
}

#[test]
fn test_direct_create_trigger_translation_before_insert_or_update_splits_trigger()
-> Result<(), Box<dyn std::error::Error>> {
    let sql = "
CREATE TABLE brands (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    edited_at TEXT
);

CREATE OR REPLACE FUNCTION set_brands_edited_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.edited_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_upsert_brands_edited_at
BEFORE INSERT OR UPDATE ON brands
FOR EACH ROW EXECUTE FUNCTION set_brands_edited_at();
";

    let translated = translate_with_direct_create_trigger_path(sql, &Pg2SqliteOptions::default())?;

    let mut connection = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;

    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }

    let trigger_definitions: Vec<TriggerDefinition> = diesel::sql_query(
        "SELECT name, sql FROM sqlite_master \
         WHERE type = 'trigger' AND name LIKE 'trigger_upsert_brands_edited_at%' ORDER BY name",
    )
    .load(&mut connection)?;

    assert_eq!(
        trigger_definitions.len(),
        2,
        "direct CreateTrigger translation should create distinct INSERT and UPDATE triggers"
    );

    let update_trigger = trigger_definitions
        .iter()
        .find(|trigger| trigger.name == "trigger_upsert_brands_edited_at")
        .expect("translated BEFORE UPDATE trigger should exist");
    assert!(
        update_trigger.sql.contains("BEFORE UPDATE OF id, name ON brands"),
        "maintenance update branch should remain BEFORE UPDATE: {}",
        update_trigger.sql
    );

    let insert_trigger = trigger_definitions
        .iter()
        .find(|trigger| trigger.name == "trigger_upsert_brands_edited_at_pg2sqlite_insert")
        .expect("translated AFTER INSERT trigger should exist");
    assert!(
        insert_trigger.sql.contains("AFTER INSERT ON brands"),
        "maintenance insert branch should be translated to AFTER INSERT: {}",
        insert_trigger.sql
    );

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

    let trigger_sql = translated
        .iter()
        .map(ToString::to_string)
        .find(|sql| sql.contains("CREATE TRIGGER trigger_update_brands_edited_at"))
        .expect("translated trigger statement should exist");
    assert!(
        trigger_sql.contains("UPDATE OF id, name ON brands_rls"),
        "maintenance trigger should target RLS table and exclude maintenance columns: {trigger_sql}"
    );

    let mut connection = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;

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
