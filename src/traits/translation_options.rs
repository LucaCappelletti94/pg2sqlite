//! Submodule defining a set of translation options.

/// Enum for defining the representation of UUIDs in `SQLite`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UuidRepresentation {
    /// Represent UUIDs as BLOBs.
    Blob,
    /// Represent UUIDs as TEXT.
    Text,
}

/// Enum for defining the version of UUIDs to generate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UuidVersion {
    /// Version 4 UUIDs (random).
    #[default]
    V4,
    /// Version 7 UUIDs (time-ordered).
    V7,
}

/// Enum representing PostgreSQL session variable patterns that can be mapped
/// to SQLite functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionVariablePattern {
    /// Matches `current_user` in PostgreSQL.
    CurrentUser,
    /// Matches `current_setting('variable_name')` in PostgreSQL.
    CurrentSetting {
        /// The name of the session variable (e.g., "app.user_id").
        name: String,
    },
}

impl std::fmt::Display for SessionVariablePattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CurrentUser => write!(f, "current_user"),
            Self::CurrentSetting { name } => write!(f, "current_setting('{name}')"),
        }
    }
}

/// A mapping from a PostgreSQL session variable pattern to a SQLite function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionVariableMapping {
    /// The PostgreSQL pattern to match.
    pub pg_pattern: SessionVariablePattern,
    /// The SQLite function name to use as replacement.
    pub sqlite_function: String,
}

impl SessionVariableMapping {
    /// Creates a new session variable mapping.
    #[must_use]
    pub fn new(pg_pattern: SessionVariablePattern, sqlite_function: impl Into<String>) -> Self {
        Self { pg_pattern, sqlite_function: sqlite_function.into() }
    }

    /// Creates a mapping for `current_user`.
    #[must_use]
    pub fn current_user(sqlite_function: impl Into<String>) -> Self {
        Self::new(SessionVariablePattern::CurrentUser, sqlite_function)
    }

    /// Creates a mapping for `current_setting('name')`.
    #[must_use]
    pub fn current_setting(name: impl Into<String>, sqlite_function: impl Into<String>) -> Self {
        Self::new(SessionVariablePattern::CurrentSetting { name: name.into() }, sqlite_function)
    }
}

/// Trait defining translation options for the  library.
pub trait TranslationOptions {
    #[must_use]
    /// Sets the option to drop check constraints containing unsupported
    fn remove_unsupported_check_constraints(self) -> Self;

    /// Returns whether to drop check constraints containing unsupported
    /// functions.
    fn should_remove_unsupported_check_constraints(&self) -> bool;

    #[must_use]
    /// Sets the option to specify the representation of UUIDs in `SQLite`.
    fn with_uuid_representation(self, representation: UuidRepresentation) -> Self;

    /// Returns the representation of UUIDs in `SQLite`.
    fn get_uuid_representation(&self) -> Option<UuidRepresentation>;

    #[must_use]
    /// Sets the option to specify the name of the function to use for UUID
    /// generation.
    fn with_uuid_function_name(self, name: String) -> Self;

    /// Returns the name of the function to use for UUID generation.
    fn get_uuid_function_name(&self) -> &str;

    // ==================== RLS Options ====================

    #[must_use]
    /// Sets the suffix to append to table names when renaming them for RLS
    /// views. Default is "_rls".
    fn with_rls_table_suffix(self, suffix: impl Into<String>) -> Self;

    /// Returns the suffix used for renamed RLS tables.
    fn get_rls_table_suffix(&self) -> &str;

    #[must_use]
    /// Sets the role name to use when filtering policies. Only policies that
    /// apply to PUBLIC or this role will be translated.
    fn with_session_user_role(self, role: impl Into<String>) -> Self;

    /// Returns the session user role for policy filtering.
    fn get_session_user_role(&self) -> Option<&str>;

    #[must_use]
    /// Adds a session variable mapping from a PostgreSQL pattern to a SQLite
    /// function.
    fn with_session_variable(self, mapping: SessionVariableMapping) -> Self;

    /// Returns all configured session variable mappings.
    fn get_session_variables(&self) -> &[SessionVariableMapping];

    /// Finds the SQLite function name for a given PostgreSQL session variable
    /// pattern. Returns `None` if no mapping is configured.
    fn find_session_variable_function(&self, pattern: &SessionVariablePattern) -> Option<&str>;

    #[must_use]
    /// Convenience method to set up a session user function that handles both
    /// `current_user` and `current_setting('variable_name')` patterns.
    fn with_session_user(
        self,
        variable_name: impl Into<String>,
        sqlite_function: impl Into<String>,
    ) -> Self;
}
