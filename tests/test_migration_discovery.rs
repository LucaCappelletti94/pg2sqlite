//! Tests for migration discovery helpers (`ups`, `ups_until`).
//!
//! `ups`/`ups_until` are filesystem-backed and only available with the
//! `std` feature.

#![cfg(feature = "std")]

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use tempfile::TempDir;

#[cfg(unix)]
#[test]
fn ups_ignores_symlink_directory_loops() {
    let temp = TempDir::new().unwrap();
    let migrations_dir = temp.path().join("migrations");
    let real_migration = migrations_dir.join("0001_create_users/up.sql");
    std::fs::create_dir_all(real_migration.parent().unwrap()).unwrap();
    std::fs::write(&real_migration, "CREATE TABLE users (id INT PRIMARY KEY);").unwrap();

    // Create a directory symlink loop inside migrations.
    let loop_link = migrations_dir.join("loop");
    std::os::unix::fs::symlink(&migrations_dir, &loop_link).unwrap();

    let result = Pg2Sqlite::ups(&migrations_dir);
    assert!(
        result.is_ok(),
        "ups() should ignore symlinked directories instead of recursing into loops, got: {result:?}"
    );

    let translated = result.unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    assert!(!translated.is_empty(), "Expected real migrations to still be discovered");
}
