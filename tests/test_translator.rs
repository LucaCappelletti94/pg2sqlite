//! Test translating the core migrations used in the `core_structures` crate.

use diesel::{Connection, RunQueryDsl, SqliteConnection, connection::SimpleConnection};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::{TranslationOptions, UuidRepresentation},
};

#[test]
/// Test translating the core migrations used in the `core_structures` crate.
fn test_translator() -> Result<(), Box<dyn std::error::Error>> {
    let translated_migrations = Pg2Sqlite::from_git(
        "https://github.com/earth-metabolome-initiative/asset-procedure-schema",
    )?
    .translate(
        &Pg2SqliteOptions::default()
            .remove_unsupported_check_constraints()
            .with_uuid_representation(UuidRepresentation::Blob),
    )?;

    // We try to parse the translated migrations using the `sqlparser` crate,
    // for the `SQLite` dialect.
    for translated_migration in &translated_migrations {
        sqlparser::parser::Parser::parse_sql(
            &sqlparser::dialect::SQLiteDialect {},
            &translated_migration.to_string(),
        )
        .expect("Failed to parse the translated migration");
    }

    // We create a testcontainer `Docker` for `SQLite` and run the translated
    // migrations, considering the severe limitations of our target use case
    // which is `WASM + SQLite`.
    let mut connection = SqliteConnection::establish(":memory:")?;

    // Enable foreign key constraints
    diesel::sql_query("PRAGMA foreign_keys = ON")
        .execute(&mut connection)
        .expect("Failed to enable foreign key constraints");

    // Enable recursive triggers
    diesel::sql_query("PRAGMA recursive_triggers = ON")
        .execute(&mut connection)
        .expect("Failed to enable recursive triggers");

    // Set journal mode to WAL for better performance
    diesel::sql_query("PRAGMA journal_mode = WAL")
        .execute(&mut connection)
        .expect("Failed to set journal mode to WAL");

    let number_of_migrations = translated_migrations.len();
    for (i, translated_migration) in
        translated_migrations.iter().enumerate().map(|(v, s)| (v + 1, s))
    {
        let sql = translated_migration.to_string();
        if let Err(err) = connection.batch_execute(&sql) {
            eprintln!(
                "Failed to run the translated statement {i}/{number_of_migrations} {sql}: {err}"
            );
        }
    }

    Ok(())
}
