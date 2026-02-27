//! Test specific trigger translation issues

use diesel::{Connection, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};

// Define Diesel schema and models

diesel::table! {
    /// Table for procedure template asset models.
    procedure_template_asset_models (id) {
        /// Primary key.
        id -> Integer,
        /// Name of the asset model.
        name -> Text,
        /// ID of the procedure template.
        procedure_template_id -> Integer,
        /// Optional based on ID.
        based_on_id -> Nullable<Integer>,
        /// Optional asset model ID.
        asset_model_id -> Nullable<Integer>,
    }
}

diesel::table! {
    /// Table for parent procedure templates.
    parent_procedure_templates (parent_id, child_id) {
        /// Parent ID.
        parent_id -> Integer,
        /// Child ID.
        child_id -> Integer,
    }
}

diesel::table! {
    /// Table for next procedure templates.
    next_procedure_templates (parent_id, predecessor_id, successor_id) {
        /// Parent ID.
        parent_id -> Integer,
        /// Predecessor ID.
        predecessor_id -> Integer,
        /// Successor ID.
        successor_id -> Integer,
    }
}

diesel::allow_tables_to_appear_in_same_query!(
    procedure_template_asset_models,
    parent_procedure_templates,
    next_procedure_templates
);

/// A procedure template asset model record for insertion.
#[derive(Insertable)]
#[diesel(table_name = procedure_template_asset_models)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct NewAssetModel<'a> {
    /// Name of the asset model.
    name: &'a str,
    /// ID of the procedure template.
    procedure_template_id: i32,
    /// Asset model ID.
    asset_model_id: i32,
}

/// A next procedure template record for insertion.
#[derive(Insertable)]
#[diesel(table_name = next_procedure_templates)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
#[allow(clippy::struct_field_names)]
struct NewNextProcedure {
    /// Parent ID.
    parent_id: i32,
    /// Predecessor ID.
    predecessor_id: i32,
    /// Successor ID.
    successor_id: i32,
}

#[test]
/// Test trigger translation for specific issues where multiple inserts or on
/// conflict occur
fn test_trigger_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/trigger_issue.sql");

    let translated_migrations = Pg2Sqlite::default()
        .sql(sql)?
        .translate(&Pg2SqliteOptions::default().remove_unsupported_check_constraints())?;

    let mut generated_sql = String::new();
    for translated_migration in &translated_migrations {
        let stmt_str = translated_migration.to_string();
        generated_sql.push_str(&stmt_str);
        generated_sql.push('\n');

        // Check validity
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::SQLiteDialect {}, &stmt_str)
            .expect("Failed to parse the translated migration");
    }

    // Verify consistency with snapshot
    insta::assert_snapshot!(generated_sql);

    // Verify it runs on SQLite
    let mut connection = SqliteConnection::establish(":memory:")?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;

    for translated_migration in translated_migrations {
        diesel::sql_query(translated_migration.to_string()).execute(&mut connection)?;
    }

    let p1 = 1;
    let c1 = 2;
    let c2 = 3;
    let a1 = 5; // asset id

    // 1. Insert initial data using Diesel DSL
    // Asset Models for c1
    diesel::insert_into(procedure_template_asset_models::table)
        .values(&NewAssetModel { name: "asset1", procedure_template_id: c1, asset_model_id: a1 })
        .execute(&mut connection)?;

    // 2. Trigger Action: Insert into next_procedure_templates (p1 -> c1 -> c2)
    diesel::insert_into(next_procedure_templates::table)
        .values(&NewNextProcedure { parent_id: p1, predecessor_id: c1, successor_id: c2 })
        .execute(&mut connection)?;

    // 3. Verifications

    // Check 'parent_procedure_templates'
    // Should have (p1, c1) and (p1, c2)
    let parents_count: i64 = parent_procedure_templates::table
        .filter(parent_procedure_templates::parent_id.eq(p1))
        .count()
        .get_result(&mut connection)?;

    assert_eq!(parents_count, 2, "Expected 2 parent-child relationships for p1");

    // Check 'procedure_template_asset_models'
    // p1 should inherit asset from c1.
    let assets_count: i64 = procedure_template_asset_models::table
        .filter(procedure_template_asset_models::procedure_template_id.eq(p1))
        .filter(procedure_template_asset_models::name.eq("asset1"))
        .count()
        .get_result(&mut connection)?;

    assert_eq!(assets_count, 1, "Expected p1 to inherit 'asset1' from c1");

    Ok(())
}
