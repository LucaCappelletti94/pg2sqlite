//! Tests for `Pg2SqliteOptions` builder methods in `src/options.rs` and
//! `src/traits/translation_options.rs`.
//!
//! Covers:
//! - Default values
//! - Chained builder with all `with_*` methods
//! - `find_session_variable_function` -> finds correct mapping
//! - `find_session_variable_function` -> returns None for unknown pattern
//! - `with_session_user` convenience -> creates both current_user and
//!   current_setting mappings

use pg2sqlite::{
    prelude::{
        Pg2SqliteOptions, SessionVariableMapping, SessionVariablePattern, UuidRepresentation,
    },
    traits::TranslationOptions,
};

// ==================== Default values ====================

#[test]
fn default_values() {
    let options = Pg2SqliteOptions::default();

    assert!(!options.should_remove_unsupported_check_constraints());
    assert!(options.get_uuid_representation().is_none());
    assert_eq!(options.get_uuid_function_name(), "uuid");
    assert_eq!(options.get_rls_table_suffix(), "_rls");
    assert!(options.get_session_user_role().is_none());
    assert!(options.get_session_variables().is_empty());
    assert!(options.get_rls_audit_table_name().is_none());
    assert!(!options.is_strict_rls_validation());
}

// ==================== Builder chaining ====================

#[test]
fn all_builder_methods_chain() {
    let options = Pg2SqliteOptions::default()
        .remove_unsupported_check_constraints()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string())
        .with_rls_table_suffix("_backing")
        .with_session_user_role("app_user")
        .with_session_variable(SessionVariableMapping::current_user("get_current_user"))
        .with_rls_audit_table_name("rls_violations")
        .with_strict_rls_validation();

    assert!(options.should_remove_unsupported_check_constraints());
    assert_eq!(options.get_uuid_representation(), Some(UuidRepresentation::Blob));
    assert_eq!(options.get_uuid_function_name(), "uuidv7");
    assert_eq!(options.get_rls_table_suffix(), "_backing");
    assert_eq!(options.get_session_user_role(), Some("app_user"));
    assert_eq!(options.get_session_variables().len(), 1);
    assert_eq!(options.get_rls_audit_table_name(), Some("rls_violations"));
    assert!(options.is_strict_rls_validation());
}

#[test]
fn uuid_text_representation() {
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text);
    assert_eq!(options.get_uuid_representation(), Some(UuidRepresentation::Text));
}

// ==================== Session variable functions ====================

#[test]
fn find_session_variable_function_current_user() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_user("get_user"));

    let result = options.find_session_variable_function(&SessionVariablePattern::CurrentUser);
    assert_eq!(result, Some("get_user"));
}

#[test]
fn find_session_variable_function_current_setting() {
    let options = Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting("app.user_id", "get_app_user"),
    );

    let result = options.find_session_variable_function(&SessionVariablePattern::CurrentSetting {
        name: "app.user_id".to_string(),
    });
    assert_eq!(result, Some("get_app_user"));
}

#[test]
fn find_session_variable_function_returns_none_for_unknown() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_user("get_user"));

    let result = options.find_session_variable_function(&SessionVariablePattern::CurrentSetting {
        name: "app.unknown".to_string(),
    });
    assert_eq!(result, None);
}

#[test]
fn find_session_variable_function_empty_options() {
    let options = Pg2SqliteOptions::default();
    let result = options.find_session_variable_function(&SessionVariablePattern::CurrentUser);
    assert_eq!(result, None);
}

// ==================== with_session_user convenience ====================

#[test]
fn with_session_user_creates_both_mappings() {
    let options = Pg2SqliteOptions::default().with_session_user("app.user_id", "current_app_user");

    let variables = options.get_session_variables();
    assert_eq!(variables.len(), 2, "with_session_user should create 2 mappings");

    // Should have a CurrentUser mapping
    let current_user_func =
        options.find_session_variable_function(&SessionVariablePattern::CurrentUser);
    assert_eq!(current_user_func, Some("current_app_user"));

    // Should have a CurrentSetting mapping
    let current_setting_func =
        options.find_session_variable_function(&SessionVariablePattern::CurrentSetting {
            name: "app.user_id".to_string(),
        });
    assert_eq!(current_setting_func, Some("current_app_user"));
}

// ==================== SessionVariableMapping constructors ====================

#[test]
fn session_variable_mapping_new() {
    let mapping = SessionVariableMapping::new(SessionVariablePattern::CurrentUser, "my_func");
    assert_eq!(mapping.pg_pattern, SessionVariablePattern::CurrentUser);
    assert_eq!(mapping.sqlite_function, "my_func");
}

#[test]
fn session_variable_mapping_current_user() {
    let mapping = SessionVariableMapping::current_user("get_user");
    assert_eq!(mapping.pg_pattern, SessionVariablePattern::CurrentUser);
    assert_eq!(mapping.sqlite_function, "get_user");
}

#[test]
fn session_variable_mapping_current_setting() {
    let mapping = SessionVariableMapping::current_setting("app.tenant_id", "get_tenant");
    assert_eq!(
        mapping.pg_pattern,
        SessionVariablePattern::CurrentSetting { name: "app.tenant_id".to_string() }
    );
    assert_eq!(mapping.sqlite_function, "get_tenant");
}

// ==================== SessionVariablePattern Display ====================

#[test]
fn session_variable_pattern_display() {
    let current_user = SessionVariablePattern::CurrentUser;
    assert_eq!(current_user.to_string(), "current_user");

    let current_setting =
        SessionVariablePattern::CurrentSetting { name: "app.user_id".to_string() };
    assert_eq!(current_setting.to_string(), "current_setting('app.user_id')");
}

// ==================== Multiple session variables ====================

#[test]
fn multiple_session_variables() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_user("get_user"))
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.tenant_id",
            "get_tenant",
        ))
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.department",
            "get_department",
        ));

    assert_eq!(options.get_session_variables().len(), 3);

    assert_eq!(
        options.find_session_variable_function(&SessionVariablePattern::CurrentUser),
        Some("get_user")
    );
    assert_eq!(
        options.find_session_variable_function(&SessionVariablePattern::CurrentSetting {
            name: "app.tenant_id".to_string()
        }),
        Some("get_tenant")
    );
    assert_eq!(
        options.find_session_variable_function(&SessionVariablePattern::CurrentSetting {
            name: "app.department".to_string()
        }),
        Some("get_department")
    );
}
