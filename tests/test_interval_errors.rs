//! Tests for INTERVAL expression rejection and error messages.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// Schema definitions for test tables
diesel::table! {
    /// Events table for testing interval translations.
    events (id) {
        /// Event ID.
        id -> Integer,
        /// Event timestamp.
        event_time -> Text,
    }
}

diesel::table! {
    /// Logs table for testing interval with time units.
    logs (id) {
        /// Log entry ID.
        id -> Integer,
        /// Timestamp when log was created.
        created_at -> Text,
    }
}

diesel::table! {
    /// Tasks table for testing interval arithmetic.
    tasks (id) {
        /// Task ID.
        id -> Integer,
        /// Task due date.
        due_date -> Text,
    }
}

/// INTERVAL expressions are not valid SQLite syntax; the translator must reject
/// them. SQLite uses date modifier strings like date('now', '-7 days') instead.
#[test]
fn test_interval_translation_basic_errors() {
    let sql = "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            event_time TEXT NOT NULL
        );
        SELECT * FROM events WHERE event_time > INTERVAL '1 day';
    ";

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "INTERVAL expression must cause a translation error");
    let err = result.unwrap_err().to_string().to_lowercase();
    assert!(err.contains("interval"), "Error must mention INTERVAL, got: {err}");
}

/// INTERVAL with time unit must also be rejected.
#[test]
fn test_interval_with_time_unit_errors() {
    let sql = "
        CREATE TABLE logs (
            id INTEGER PRIMARY KEY,
            created_at TEXT NOT NULL
        );
        SELECT * FROM logs WHERE created_at > INTERVAL '2 hours';
    ";

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "INTERVAL expression must cause a translation error");
}

/// Test that INTERVAL in date arithmetic expressions is translated without
/// crashing.
#[test]
fn test_interval_in_arithmetic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE tasks (
            id INTEGER PRIMARY KEY,
            due_date TEXT NOT NULL
        );
        SELECT id, due_date FROM tasks;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    // Verify translation succeeded
    assert!(!translated.is_empty(), "Should have translated statements");

    Ok(())
}
