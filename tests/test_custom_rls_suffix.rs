//! Tests for the `with_rls_table_suffix()` option.
//!
//! All existing RLS tests use the default `_rls` suffix.  These tests verify
//! that a custom suffix (e.g. `_secure`) is applied consistently to:
//! - The backing table name
//! - Foreign-key references pointing at the backing table
//! - View and trigger names derived from the backing table

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation},
    traits::TranslationOptions,
};
use rusqlite::Connection;

fn make_options_with_suffix(suffix: &str) -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string())
        .with_rls_table_suffix(suffix)
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
}

/// The backing table must carry the custom suffix; the view retains the
/// original table name.
#[test]
fn test_custom_suffix_renames_backing_table() {
    let sql = include_str!("fixtures/rls_basic.sql");
    let options = make_options_with_suffix("_secure");

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    // Backing table should carry the custom suffix
    assert!(
        sql_output.contains("documents_secure"),
        "Backing table should use '_secure' suffix, got:\n{sql_output}"
    );
    // Default suffix must NOT appear
    assert!(
        !sql_output.contains("documents_rls"),
        "Backing table must not carry the default '_rls' suffix, got:\n{sql_output}"
    );
    // View must keep the original name (without any suffix)
    assert!(
        sql_output.contains("CREATE VIEW documents "),
        "View should still be named 'documents' (no suffix), got:\n{sql_output}"
    );
    let conn = Connection::open_in_memory().unwrap();
    for stmt in &translated {
        conn.execute_batch(&format!("{stmt};")).unwrap();
    }
}

/// Snapshot of the full translation output with a custom suffix so that any
/// future regression is immediately visible.
#[test]
fn test_custom_suffix_snapshot() {
    let sql = include_str!("fixtures/rls_basic.sql");
    let options = make_options_with_suffix("_secure");

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    insta::assert_snapshot!("custom_rls_suffix_translation", sql_output);
}

/// Foreign-key references pointing at an RLS table must be updated to use the
/// custom suffix, not the default `_rls`.
#[test]
fn test_custom_suffix_in_foreign_keys() {
    let sql = include_str!("fixtures/rls_fk_simple.sql");
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string())
        .with_rls_table_suffix("_secure")
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_session_variable(SessionVariableMapping::current_user("current_app_username"));

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    // FK should reference the custom-suffixed backing table
    assert!(
        sql_output.contains("REFERENCES users_secure"),
        "FK should reference 'users_secure' (custom suffix), got:\n{sql_output}"
    );
    // Default suffix must NOT appear in any FK
    assert!(
        !sql_output.contains("REFERENCES users_rls"),
        "FK must not reference 'users_rls' when custom suffix is set, got:\n{sql_output}"
    );
    let conn = Connection::open_in_memory().unwrap();
    for stmt in &translated {
        conn.execute_batch(&format!("{stmt};")).unwrap();
    }
}
