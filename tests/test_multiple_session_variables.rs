//! Tests for RLS policies that reference multiple different session variables.
//!
//! Each existing RLS test uses a single session-variable mapping.  This module
//! verifies that two distinct mappings — one for
//! `current_setting('app.user_id')` (binary UUID) and one for `current_user`
//! (text username) — are applied independently and correctly within the same
//! policy.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation},
    traits::TranslationOptions,
};

fn make_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string())
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        // UUID-based identity mapping
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        // Username-based identity mapping
        .with_session_variable(SessionVariableMapping::current_user("current_app_username"))
}

/// Snapshot of the translation output for a policy that uses both
/// `current_setting('app.user_id')` and `current_user`.
#[test]
fn test_multiple_session_vars_snapshot() {
    let sql = include_str!("fixtures/rls_multi_session_vars.sql");
    let options = make_options();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    insta::assert_snapshot!("multiple_session_vars_translation", sql_output);
}

/// Verify that both session-variable patterns are replaced with their
/// configured SQLite function names in the generated SQL.
#[test]
fn test_both_session_variable_patterns_are_mapped() {
    let sql = include_str!("fixtures/rls_multi_session_vars.sql");
    let options = make_options();

    let translated = Pg2Sqlite::default().sql(sql).unwrap().translate(&options).unwrap();
    let sql_output = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    // current_setting('app.user_id') → current_app_user()
    assert!(
        sql_output.contains("current_app_user()"),
        "current_setting('app.user_id') should be mapped to current_app_user(), got:\n{sql_output}"
    );
    // current_user → current_app_username()
    assert!(
        sql_output.contains("current_app_username()"),
        "current_user should be mapped to current_app_username(), got:\n{sql_output}"
    );
    // No raw current_setting(...) references should remain
    assert!(
        !sql_output.contains("current_setting"),
        "All current_setting() references should be replaced, got:\n{sql_output}"
    );
}

/// When only one of the two required session-variable mappings is provided,
/// translation should fail with a clear error about the missing mapping.
#[test]
fn test_missing_session_variable_for_current_setting_fails() {
    let sql = include_str!("fixtures/rls_multi_session_vars.sql");

    // Provide ONLY the current_user mapping; omit current_setting('app.user_id')
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string())
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_user("current_app_username"));

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);

    assert!(
        result.is_err(),
        "Translation should fail when current_setting('app.user_id') mapping is absent"
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("session variable mapping") || error_msg.contains("current_setting"),
        "Error should mention the missing session variable mapping, got: {error_msg}"
    );
}
