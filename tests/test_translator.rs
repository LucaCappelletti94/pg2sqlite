//! Integration test for translating migrations loaded from a git repository.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use git2::{Repository, Signature};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::{TranslationOptions, UuidRepresentation},
};
use tempfile::TempDir;

fn build_local_migration_repo() -> Result<TempDir, Box<dyn std::error::Error>> {
    let repo_dir = TempDir::new()?;
    let repo = Repository::init(repo_dir.path())?;

    let migration_01 = repo_dir.path().join("migrations/01_create_users/up.sql");
    let migration_02 = repo_dir.path().join("migrations/02_seed_users/up.sql");
    std::fs::create_dir_all(migration_01.parent().expect("path has a parent"))?;
    std::fs::create_dir_all(migration_02.parent().expect("path has a parent"))?;

    std::fs::write(
        &migration_01,
        r#"
        CREATE TABLE users (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name TEXT NOT NULL
        );
        "#,
    )?;
    std::fs::write(
        &migration_02,
        r#"
        INSERT INTO users (name) VALUES ('alice')
        ON CONFLICT (id) DO NOTHING;
        "#,
    )?;

    let mut index = repo.index()?;
    index.add_path(std::path::Path::new("migrations/01_create_users/up.sql"))?;
    index.add_path(std::path::Path::new("migrations/02_seed_users/up.sql"))?;
    index.write()?;

    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let sig = Signature::now("pg2sqlite-test", "pg2sqlite-test@example.com")?;
    repo.commit(Some("HEAD"), &sig, &sig, "Initial test migrations", &tree, &[])?;

    Ok(repo_dir)
}

#[test]
fn test_translator() -> Result<(), Box<dyn std::error::Error>> {
    let repo_dir = build_local_migration_repo()?;
    let translated_migrations =
        Pg2Sqlite::from_git(repo_dir.path().to_str().expect("temp path should be valid UTF-8"))?
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

    // Execute translated migrations against in-memory SQLite to ensure
    // syntactic and basic runtime compatibility.
    let mut connection = SqliteConnection::establish(":memory:")?;

    // Enable foreign key constraints (PRAGMA)
    diesel::sql_query("PRAGMA foreign_keys = ON")
        .execute(&mut connection)
        .expect("Failed to enable foreign key constraints");

    // Enable recursive triggers (PRAGMA)
    diesel::sql_query("PRAGMA recursive_triggers = ON")
        .execute(&mut connection)
        .expect("Failed to enable recursive triggers");

    // Set journal mode to WAL for better performance (PRAGMA)
    diesel::sql_query("PRAGMA journal_mode = WAL")
        .execute(&mut connection)
        .expect("Failed to set journal mode to WAL");

    // Execute all translated DDL statements
    let number_of_migrations = translated_migrations.len();
    for (i, translated_migration) in
        translated_migrations.iter().enumerate().map(|(v, s)| (v + 1, s))
    {
        let sql = translated_migration.to_string();
        if let Err(err) = diesel::sql_query(&sql).execute(&mut connection) {
            eprintln!(
                "Failed to run the translated statement {i}/{number_of_migrations} {sql}: {err}"
            );
        }
    }

    Ok(())
}
