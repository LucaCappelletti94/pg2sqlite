//! Tests for the `Pg2Sqlite` struct methods.

use std::io::Write;

use pg2sqlite::{options::Pg2SqliteOptions, pg2sqlite::Pg2Sqlite};
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
