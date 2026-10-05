//! Test for PL/pgSQL triggers with IF/ELSIF/ELSE blocks.

#![allow(dead_code)]

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        audit_log (id) {
            id -> Integer,
            action -> Text,
            severity -> Text,
        }
    }

    diesel::table! {
        events (id) {
            id -> Integer,
            event_type -> Text,
            data -> Nullable<Text>,
        }
    }
}

use schema::{audit_log, events};

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = audit_log)]
struct AuditLog {
    id: i32,
    action: String,
    severity: String,
}

#[derive(Insertable, Debug)]
#[diesel(table_name = events)]
struct NewEvent {
    event_type: String,
    data: Option<String>,
}

fn translate_and_setup() -> Result<diesel::SqliteConnection, Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/trigger_elsif_else.sql");

    let options = Pg2SqliteOptions::default();

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;

    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    Ok(connection)
}

/// Snapshot test for ELSIF/ELSE translation.
#[test]
fn test_elsif_else_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/trigger_elsif_else.sql");

    let options = Pg2SqliteOptions::default();

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("trigger_elsif_else_translation", translated_sql);

    Ok(())
}

#[test]
fn test_if_branch_error() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    diesel::insert_into(events::table)
        .values(NewEvent { event_type: "error".to_string(), data: None })
        .execute(&mut connection)?;

    let logs = audit_log::table.select(AuditLog::as_select()).load(&mut connection)?;
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].action, "error_event");
    assert_eq!(logs[0].severity, "high");

    Ok(())
}

#[test]
fn test_elsif_branch_warning() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    diesel::insert_into(events::table)
        .values(NewEvent { event_type: "warning".to_string(), data: None })
        .execute(&mut connection)?;

    let logs = audit_log::table.select(AuditLog::as_select()).load(&mut connection)?;
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].action, "warning_event");
    assert_eq!(logs[0].severity, "medium");

    Ok(())
}

#[test]
fn test_elsif_branch_info() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    diesel::insert_into(events::table)
        .values(NewEvent { event_type: "info".to_string(), data: None })
        .execute(&mut connection)?;

    let logs = audit_log::table.select(AuditLog::as_select()).load(&mut connection)?;
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].action, "info_event");
    assert_eq!(logs[0].severity, "low");

    Ok(())
}

#[test]
fn test_else_branch_unknown() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    diesel::insert_into(events::table)
        .values(NewEvent { event_type: "other".to_string(), data: None })
        .execute(&mut connection)?;

    let logs = audit_log::table.select(AuditLog::as_select()).load(&mut connection)?;
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].action, "unknown_event");
    assert_eq!(logs[0].severity, "unknown");

    Ok(())
}

#[test]
fn test_multiple_events() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    diesel::insert_into(events::table)
        .values(&[
            NewEvent { event_type: "error".to_string(), data: None },
            NewEvent { event_type: "warning".to_string(), data: None },
            NewEvent { event_type: "debug".to_string(), data: None },
        ])
        .execute(&mut connection)?;

    let logs = audit_log::table.select(AuditLog::as_select()).load(&mut connection)?;
    assert_eq!(logs.len(), 3);

    let actions: Vec<_> = logs.iter().map(|l| l.action.as_str()).collect();
    assert!(actions.contains(&"error_event"));
    assert!(actions.contains(&"warning_event"));
    assert!(actions.contains(&"unknown_event")); // "debug" falls to ELSE

    Ok(())
}
