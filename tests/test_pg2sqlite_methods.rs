//! Tests for the `Pg2Sqlite` struct methods.
//!
//! Exercises the filesystem-backed loaders (`file`, `ups`, `ups_until`) and
//! their downstream translation, which all depend on the `std` feature.

#![cfg(feature = "std")]

use std::io::Write;

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::{options::Pg2SqliteOptions, pg2sqlite::Pg2Sqlite, traits::TranslationOptions};
use tempfile::{NamedTempFile, TempDir};

#[test]
fn test_statement() {
    let sql = "CREATE TABLE test_statement (id INT);";
    let statement =
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, sql)
            .unwrap()
            .pop()
            .unwrap();

    let translator = Pg2Sqlite::default().statement(statement);
    let result = translator.translate(&Pg2SqliteOptions::default()).unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0].to_string().contains("CREATE TABLE test_statement"));
}

#[test]
fn test_sql() {
    let translator = Pg2Sqlite::default().sql("CREATE TABLE test_sql (id INT);").unwrap();
    let result = translator.translate(&Pg2SqliteOptions::default()).unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0].to_string().contains("CREATE TABLE test_sql"));
}

#[test]
fn test_file() {
    let mut file = NamedTempFile::new().unwrap();
    write!(file, "CREATE TABLE test_file (id INT);").unwrap();
    let path = file.path();

    let translator = Pg2Sqlite::default().file(path).unwrap();
    let result = translator.translate(&Pg2SqliteOptions::default()).unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0].to_string().contains("CREATE TABLE test_file"));
}

#[test]
fn test_ups() {
    let dir = TempDir::new().unwrap();

    // Create nested structure:
    // dir/
    //   01/up.sql
    //   02/up.sql

    let dir1 = dir.path().join("01");
    std::fs::create_dir(&dir1).unwrap();
    let file1_path = dir1.join("up.sql");
    let mut file1 = std::fs::File::create(&file1_path).unwrap();
    write!(file1, "CREATE TABLE t1 (id INT);").unwrap();

    let dir2 = dir.path().join("02");
    std::fs::create_dir(&dir2).unwrap();
    let file2_path = dir2.join("up.sql");
    let mut file2 = std::fs::File::create(file2_path).unwrap();
    write!(file2, "CREATE TABLE t2 (id INT);").unwrap();

    let translator = Pg2Sqlite::ups(dir.path()).unwrap();
    let result = translator.translate(&Pg2SqliteOptions::default()).unwrap();

    // Should have processed 2 files, so 2 statements
    assert_eq!(result.len(), 2);
    // Because of alphabetical sorting, t1 comes first
    assert!(result[0].to_string().contains("CREATE TABLE t1"));
    assert!(result[1].to_string().contains("CREATE TABLE t2"));
}

#[test]
fn test_ups_until() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    // Create structure:
    // 01/up.sql
    // 02/up.sql
    // 03/up.sql

    let dir1 = root.join("01");
    std::fs::create_dir(&dir1).unwrap();
    let file1_path = dir1.join("up.sql");
    std::fs::write(&file1_path, "CREATE TABLE t1 (id INT);").unwrap();

    let dir2 = root.join("02");
    std::fs::create_dir(&dir2).unwrap();
    let file2_path = dir2.join("up.sql");
    std::fs::write(&file2_path, "CREATE TABLE t2 (id INT);").unwrap();

    let dir3 = root.join("03");
    std::fs::create_dir(&dir3).unwrap();
    let file3_path = dir3.join("up.sql");
    std::fs::write(&file3_path, "CREATE TABLE t3 (id INT);").unwrap();

    // Test stopping at the second one
    let translator = Pg2Sqlite::ups_until(root, &file2_path).unwrap();
    let result = translator.translate(&Pg2SqliteOptions::default()).unwrap();

    assert_eq!(result.len(), 2); // Should have t1 and t2
    assert!(result[0].to_string().contains("CREATE TABLE t1"));
    assert!(result[1].to_string().contains("CREATE TABLE t2"));
}

#[test]
fn test_ups_until_stop_not_found() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    let dir1 = root.join("01");
    std::fs::create_dir(&dir1).unwrap();
    std::fs::write(dir1.join("up.sql"), "CREATE TABLE t1 (id INT);").unwrap();

    let dir2 = root.join("02");
    std::fs::create_dir(&dir2).unwrap();
    std::fs::write(dir2.join("up.sql"), "CREATE TABLE t2 (id INT);").unwrap();

    // Existing file, but not a discovered up.sql migration path.
    let stop_at = root.join("not_a_migration.sql");
    std::fs::write(&stop_at, "-- not an up migration").unwrap();

    let result = Pg2Sqlite::ups_until(root, &stop_at);
    assert!(result.is_err(), "Expected ups_until to fail when stop_at is not in up.sql set");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not found among discovered up.sql migrations"),
        "Expected not-found migration error, got: {err}"
    );
}

#[test]
fn test_ups_until_last_migration_matches_ups() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    let dir1 = root.join("01");
    std::fs::create_dir(&dir1).unwrap();
    std::fs::write(dir1.join("up.sql"), "CREATE TABLE t1 (id INT);").unwrap();

    let dir2 = root.join("02");
    std::fs::create_dir(&dir2).unwrap();
    std::fs::write(dir2.join("up.sql"), "CREATE TABLE t2 (id INT);").unwrap();

    let dir3 = root.join("03");
    std::fs::create_dir(&dir3).unwrap();
    let last_path = dir3.join("up.sql");
    std::fs::write(&last_path, "CREATE TABLE t3 (id INT);").unwrap();

    let all = Pg2Sqlite::ups(root).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    let until_last = Pg2Sqlite::ups_until(root, &last_path)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap();

    let all_sql = all.iter().map(ToString::to_string).collect::<Vec<_>>();
    let until_last_sql = until_last.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(all_sql, until_last_sql);
}

#[test]
fn test_translate() {
    // Basic test of translate flow, more detailed translation tests are elsewhere
    let translator = Pg2Sqlite::default().sql("CREATE TABLE test_trans (id INT);").unwrap();
    let result = translator.translate(&Pg2SqliteOptions::default());
    assert!(result.is_ok());
    let statements = result.unwrap();
    assert_eq!(statements.len(), 1);
}

#[test]
fn test_on_conflict_do_update() {
    // Test that ON CONFLICT DO UPDATE is properly translated (pass-through)
    let sql = "
        CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT);
        INSERT INTO kv (key, value) VALUES ('a', 'b')
        ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value;
    ";
    let translator = Pg2Sqlite::default().sql(sql).unwrap();
    let result = translator.translate(&Pg2SqliteOptions::default()).unwrap();
    assert_eq!(result.len(), 2);

    let insert_sql = result[1].to_string();
    assert!(insert_sql.contains("ON CONFLICT"), "Should contain ON CONFLICT");
    assert!(insert_sql.contains("DO UPDATE"), "Should contain DO UPDATE");
    assert!(insert_sql.contains("EXCLUDED.value"), "Should contain EXCLUDED reference");

    // Verify it parses as valid SQLite
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::SQLiteDialect {}, &insert_sql)
        .expect("INSERT with ON CONFLICT DO UPDATE should be valid SQLite");
}

#[test]
fn test_alter_table_add_column_is_translated() {
    let stmts = Pg2Sqlite::default()
        .sql("CREATE TABLE t (id INT PRIMARY KEY); ALTER TABLE t ADD COLUMN name TEXT;")
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap();
    assert_eq!(stmts.len(), 2, "ADD COLUMN must be emitted alongside CREATE TABLE");
    assert!(stmts[1].to_string().contains("ADD COLUMN name TEXT"), "got: {}", stmts[1]);
}

#[test]
fn test_missing_trigger_body_errors() {
    let sql = r#"
        CREATE TABLE docs(id INTEGER PRIMARY KEY);
        CREATE FUNCTION docs_trigger_fn() RETURNS trigger LANGUAGE plpgsql;
        CREATE TRIGGER docs_ai
        AFTER INSERT ON docs
        FOR EACH ROW
        EXECUTE FUNCTION docs_trigger_fn();
    "#;
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "Missing trigger body should error");
    assert!(result.unwrap_err().to_string().contains("Trigger function"));
}

#[test]
fn test_index_skipped_for_role_without_access() {
    let sql = r#"
        CREATE ROLE app_user;
        CREATE TABLE private_docs(id INTEGER PRIMARY KEY, title TEXT);
        CREATE INDEX private_docs_title_idx ON private_docs(title);
    "#;
    let options = Pg2SqliteOptions::default().with_session_user_role("app_user");
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    assert!(
        stmts.iter().all(|s| !s.to_string().contains("CREATE INDEX private_docs_title_idx")),
        "Index should be filtered for role without SELECT"
    );
}

/// `translate_to_sql` must return `Vec<String>` of SQL text directly.
#[test]
fn translate_to_sql_returns_strings() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE convenience_test (id INTEGER PRIMARY KEY, val TEXT);";
    let sql_strings =
        Pg2Sqlite::default().sql(sql)?.translate_to_sql(&Pg2SqliteOptions::default())?;
    assert!(!sql_strings.is_empty(), "translate_to_sql must return at least one statement");
    assert!(
        sql_strings[0].to_uppercase().contains("CREATE TABLE"),
        "First string must be the CREATE TABLE, got: {}",
        sql_strings[0]
    );

    // The returned strings must be valid SQLite.
    let mut conn = SqliteConnection::establish(":memory:")?;
    for sql_str in &sql_strings {
        diesel::sql_query(sql_str).execute(&mut conn)?;
    }
    Ok(())
}
