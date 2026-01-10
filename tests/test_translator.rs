//! Test translating the core migrations used in the `core_structures` crate.

use diesel::{Connection, SqliteConnection, connection::SimpleConnection};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};

#[test]
/// Test translating the core migrations used in the `core_structures` crate.
fn test_translator() -> Result<(), Box<dyn std::error::Error>> {
    let translated_migrations = Pg2Sqlite::from_git(
        "https://github.com/earth-metabolome-initiative/asset-procedure-schema",
    )?
    .translate(&Pg2SqliteOptions::default().remove_unsupported_check_constraints())?;

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

    let number_of_migrations = translated_migrations.len();
    for (i, translated_migration) in
        translated_migrations.iter().enumerate().map(|(v, s)| (v + 1, s))
    {
        let sql = translated_migration.to_string();
        if let Err(err) = connection.batch_execute(&sql) {
            panic!("Failed to run the translated statement {i}/{number_of_migrations} {sql}: {err}")
        }
    }

    Ok(())
}
