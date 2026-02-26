//! Submodule defining a struct providing options for the translation.

use crate::traits::{
    SessionVariableMapping, SessionVariablePattern, TranslationOptions, UuidRepresentation,
};

/// Struct to hold options for the translation.
#[derive(Debug, Clone)]
pub struct Pg2SqliteOptions {
    /// Whether to drop check constraints containing unsupported functions.
    remove_unsupported_check_constraints: bool,
    /// The representation of UUIDs in `SQLite`.
    uuid_representation: Option<UuidRepresentation>,
    /// The name of the function to use for UUID generation.
    uuid_function_name: String,
    /// The suffix to append to table names when renaming them for RLS views.
    rls_table_suffix: String,
    /// The role name to use when filtering policies.
    session_user_role: Option<String>,
    /// Mappings from PostgreSQL session variable patterns to SQLite functions.
    session_variables: Vec<SessionVariableMapping>,
    /// The name of the audit table for RLS validation monitoring.
    rls_audit_table_name: Option<String>,
    /// Whether to enable strict RLS validation (abort on violations).
    strict_rls_validation: bool,
}

impl Default for Pg2SqliteOptions {
    fn default() -> Self {
        Self {
            remove_unsupported_check_constraints: false,
            uuid_representation: None,
            uuid_function_name: "uuid".to_string(),
            rls_table_suffix: "_rls".to_string(),
            session_user_role: None,
            session_variables: Vec::new(),
            rls_audit_table_name: None,
            strict_rls_validation: false,
        }
    }
}


impl TranslationOptions for Pg2SqliteOptions {
    fn remove_unsupported_check_constraints(mut self) -> Self {
        self.remove_unsupported_check_constraints = true;
        self
    }

    fn should_remove_unsupported_check_constraints(&self) -> bool {
        self.remove_unsupported_check_constraints
    }

    fn with_uuid_representation(mut self, representation: UuidRepresentation) -> Self {
        self.uuid_representation = Some(representation);
        self
    }

    fn get_uuid_representation(&self) -> Option<UuidRepresentation> {
        self.uuid_representation
    }

    fn with_uuid_function_name(mut self, name: impl Into<String>) -> Self {
        self.uuid_function_name = name.into();
        self
    }

    fn get_uuid_function_name(&self) -> &str {
        &self.uuid_function_name
    }

    // ==================== RLS Options ====================

    fn with_rls_table_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.rls_table_suffix = suffix.into();
        self
    }

    fn get_rls_table_suffix(&self) -> &str {
        &self.rls_table_suffix
    }

    fn with_session_user_role(mut self, role: impl Into<String>) -> Self {
        self.session_user_role = Some(role.into());
        self
    }

    fn get_session_user_role(&self) -> Option<&str> {
        self.session_user_role.as_deref()
    }

    fn with_session_variable(mut self, mapping: SessionVariableMapping) -> Self {
        self.session_variables.push(mapping);
        self
    }

    fn get_session_variables(&self) -> &[SessionVariableMapping] {
        &self.session_variables
    }

    fn find_session_variable_function(&self, pattern: &SessionVariablePattern) -> Option<&str> {
        self.session_variables
            .iter()
            .rev()
            .find(|m| &m.pg_pattern == pattern)
            .map(|m| m.sqlite_function.as_str())
    }

    fn with_session_user(
        self,
        variable_name: impl Into<String>,
        sqlite_function: impl Into<String>,
    ) -> Self {
        let func_name = sqlite_function.into();
        self.with_session_variable(SessionVariableMapping::current_user(func_name.clone()))
            .with_session_variable(SessionVariableMapping::current_setting(
                variable_name,
                func_name,
            ))
    }

    // ==================== RLS Validation Options ====================

    fn with_rls_audit_table_name(mut self, name: impl Into<String>) -> Self {
        self.rls_audit_table_name = Some(name.into());
        self
    }

    fn get_rls_audit_table_name(&self) -> Option<&str> {
        self.rls_audit_table_name.as_deref()
    }

    fn with_strict_rls_validation(mut self) -> Self {
        self.strict_rls_validation = true;
        self
    }

    fn is_strict_rls_validation(&self) -> bool {
        self.strict_rls_validation
    }

}
