//! Tests for `Pg2SqliteOptions` builder methods in `src/options.rs` and
//! `src/traits/translation_options.rs`.
//!
//! Covers:
//! - Default values
//! - Chained builder with all `with_*` methods
//! - `find_session_variable` -> finds correct mapping
//! - `find_session_variable` -> returns None for unknown pattern
//! - `with_session_user` convenience -> creates both current_user and
//!   current_setting mappings
//! - the type a mapping records, and what an unreadable spelling does

use pg2sqlite::{
    errors::Error,
    prelude::{
        Pg2SqliteOptions, SessionVariableMapping, SessionVariablePattern, UuidRepresentation,
    },
    traits::TranslationOptions,
};
use sqlparser::ast::{DataType, ExactNumberInfo};

/// The paired SQLite function, which is what most of these tests assert.
fn paired_function<'a>(
    options: &'a Pg2SqliteOptions,
    pattern: &SessionVariablePattern,
) -> Option<&'a str> {
    options.find_session_variable(pattern).map(|mapping| mapping.sqlite_function.as_str())
}

#[test]
fn default_values() {
    let options = Pg2SqliteOptions::default();

    assert!(!options.should_remove_unsupported_check_constraints());
    assert!(options.get_uuid_representation().is_none());
    assert_eq!(options.get_uuid_function_name(), "uuid");
    assert_eq!(options.get_rls_table_suffix(), "_rls");
    assert!(options.get_session_user_role().is_none());
    assert!(
        options.get_session_variables().is_empty(),
        "a default options carries no pairing, got {:?}",
        options.get_session_variables()
    );
    assert!(options.get_rls_audit_table_name().is_none());
    assert!(!options.is_strict_rls_validation());
}

#[test]
fn all_builder_methods_chain() {
    let options = Pg2SqliteOptions::default()
        .remove_unsupported_check_constraints()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("app_uuid".to_string())
        .with_uuid_v7_function_name("uuid7")
        .with_rls_table_suffix("_backing")
        .with_session_user_role("app_user")
        .with_session_variable(SessionVariableMapping::current_user("get_current_user"))
        .with_rls_audit_table_name("rls_violations")
        .with_strict_rls_validation();

    assert!(options.should_remove_unsupported_check_constraints());
    assert_eq!(options.get_uuid_representation(), Some(UuidRepresentation::Blob));
    assert_eq!(options.get_uuid_function_name(), "app_uuid");
    assert_eq!(options.get_uuid_v7_function_name(), Some("uuid7"));
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

#[test]
fn find_session_variable_current_user() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_user("get_user"));

    let result = paired_function(&options, &SessionVariablePattern::CurrentUser);
    assert_eq!(result, Some("get_user"));
}

#[test]
fn find_session_variable_current_setting() {
    let options = Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting("app.user_id", "get_app_user"),
    );

    let result = paired_function(
        &options,
        &SessionVariablePattern::CurrentSetting { name: "app.user_id".to_string() },
    );
    assert_eq!(result, Some("get_app_user"));
}

#[test]
fn find_session_variable_returns_none_for_unknown() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_user("get_user"));

    let result = paired_function(
        &options,
        &SessionVariablePattern::CurrentSetting { name: "app.unknown".to_string() },
    );
    assert_eq!(result, None);
}

#[test]
fn find_session_variable_empty_options() {
    let options = Pg2SqliteOptions::default();
    let result = paired_function(&options, &SessionVariablePattern::CurrentUser);
    assert_eq!(result, None);
}

#[test]
fn with_session_user_creates_both_mappings() {
    let options = Pg2SqliteOptions::default().with_session_user("app.user_id", "current_app_user");

    let variables = options.get_session_variables();
    assert_eq!(variables.len(), 2, "with_session_user should create 2 mappings");

    // Should have a CurrentUser mapping
    let current_user_func = paired_function(&options, &SessionVariablePattern::CurrentUser);
    assert_eq!(current_user_func, Some("current_app_user"));

    // Should have a CurrentSetting mapping
    let current_setting_func = paired_function(
        &options,
        &SessionVariablePattern::CurrentSetting { name: "app.user_id".to_string() },
    );
    assert_eq!(current_setting_func, Some("current_app_user"));
}

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

#[test]
fn session_variable_pattern_display() {
    let current_user = SessionVariablePattern::CurrentUser;
    assert_eq!(current_user.to_string(), "current_user");

    let current_setting =
        SessionVariablePattern::CurrentSetting { name: "app.user_id".to_string() };
    assert_eq!(current_setting.to_string(), "current_setting('app.user_id')");
}

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

    assert_eq!(paired_function(&options, &SessionVariablePattern::CurrentUser), Some("get_user"));
    assert_eq!(
        paired_function(
            &options,
            &SessionVariablePattern::CurrentSetting { name: "app.tenant_id".to_string() }
        ),
        Some("get_tenant")
    );
    assert_eq!(
        paired_function(
            &options,
            &SessionVariablePattern::CurrentSetting { name: "app.department".to_string() }
        ),
        Some("get_department")
    );
}

#[test]
fn duplicate_session_variable_mapping_last_wins() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_user("old_user_func"))
        .with_session_variable(SessionVariableMapping::current_user("new_user_func"));

    assert_eq!(
        paired_function(&options, &SessionVariablePattern::CurrentUser),
        Some("new_user_func"),
        "latest mapping should override previous mapping for the same pattern"
    );
}

#[test]
fn a_mapping_records_no_type_by_default() {
    let mapping = SessionVariableMapping::current_setting("app.user_id", "app_user_id");

    assert!(mapping.pg_type.is_none());
    assert_eq!(
        mapping.pg_type_node().expect("no recorded type is not an error"),
        None,
        "a mapping without a recorded type asks for no cast"
    );
}

#[test]
fn a_recorded_type_reads_back_as_a_node() {
    let uuid =
        SessionVariableMapping::current_setting("app.user_id", "app_user_id").with_pg_type("uuid");
    assert_eq!(uuid.pg_type_node().expect("uuid parses"), Some(DataType::Uuid));

    let scaled = SessionVariableMapping::current_user("app_user").with_pg_type("NUMERIC(10, 2)");
    assert_eq!(
        scaled.pg_type_node().expect("a parameterised type parses"),
        Some(DataType::Numeric(ExactNumberInfo::PrecisionAndScale(10, 2))),
        "the precision and scale survive, so the cast the reverse writes is the recorded one"
    );
}

#[test]
fn a_recorded_type_that_is_not_one_refuses() {
    let mapping =
        SessionVariableMapping::current_setting("app.user_id", "app_user_id").with_pg_type("123");

    let error = mapping.pg_type_node().expect_err("a number is not a type");
    assert!(
        matches!(&error, Error::SessionVariableTypeUnreadable { pattern, pg_type, source }
            if pattern == "current_setting('app.user_id')" && pg_type == "123" && source.is_some()),
        "the refusal names the pattern and the spelling, got: {error}"
    );
}

#[test]
fn a_recorded_type_with_input_left_over_refuses() {
    let mapping = SessionVariableMapping::current_setting("app.user_id", "app_user_id")
        .with_pg_type("uuid oops");

    let error = mapping.pg_type_node().expect_err("a type followed by junk is not a type");
    assert!(
        matches!(&error, Error::SessionVariableTypeUnreadable { pg_type, source, .. }
            if pg_type == "uuid oops" && source.is_none()),
        "the leftover input is refused rather than truncated to `uuid`, got: {error}"
    );
}
