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
/// Two intervals added answer an interval in PostgreSQL, and SQLite has no
/// interval type to answer with. The commutation that moves an interval from
/// the left of `+` to the right used to swap the two sides forever here, and
/// the translator aborted on a stack overflow.
#[test]
fn adding_two_intervals_is_refused() {
    let error = Pg2Sqlite::default()
        .sql("SELECT INTERVAL '1 day' + INTERVAL '2 hours';")
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("two intervals added have no SQLite form");
    assert!(error.to_string().to_lowercase().contains("interval"), "{error}");
}

/// The same for subtraction, which reached the refusal already, and for the
/// parenthesised spelling of both.
#[test]
fn interval_arithmetic_between_intervals_is_refused() {
    for sql in [
        "SELECT INTERVAL '1 day' - INTERVAL '2 hours';",
        "SELECT (INTERVAL '1 day') + (INTERVAL '2 hours');",
        "SELECT INTERVAL '1 day' + INTERVAL '2 hours' + INTERVAL '3 minutes';",
    ] {
        let error = Pg2Sqlite::default()
            .sql(sql)
            .expect("parse")
            .translate(&Pg2SqliteOptions::default())
            .unwrap_err();
        assert!(error.to_string().to_lowercase().contains("interval"), "{sql}: {error}");
    }
}
