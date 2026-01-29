//! Submodule defining a struct providing options for the translation.

use crate::traits::{TranslationOptions, UuidRepresentation};

/// Struct to hold options for the translation.
#[derive(Debug, Clone)]
pub struct Pg2SqliteOptions {
    /// Whether to drop check constraints containing unsupported functions.
    remove_unsupported_check_constraints: bool,
    /// The representation of UUIDs in SQLite.
    uuid_representation: Option<UuidRepresentation>,
    /// Whether to use pure SQL for UUID generation.
    use_pure_sql_for_uuid: bool,
    /// The name of the function to use for UUID generation (if not using pure
    /// SQL).
    uuid_function_name: String,
}

impl Default for Pg2SqliteOptions {
    fn default() -> Self {
        Self {
            remove_unsupported_check_constraints: false,
            uuid_representation: None,
            use_pure_sql_for_uuid: false,
            uuid_function_name: "uuid".to_string(),
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

    fn use_pure_sql_for_uuid(mut self, yes: bool) -> Self {
        self.use_pure_sql_for_uuid = yes;
        self
    }

    fn should_use_pure_sql_for_uuid(&self) -> bool {
        self.use_pure_sql_for_uuid
    }

    fn with_uuid_representation(mut self, representation: UuidRepresentation) -> Self {
        self.uuid_representation = Some(representation);
        self
    }

    fn get_uuid_representation(&self) -> Option<UuidRepresentation> {
        self.uuid_representation
    }

    fn with_uuid_function_name(mut self, name: String) -> Self {
        self.uuid_function_name = name;
        self
    }

    fn get_uuid_function_name(&self) -> &str {
        &self.uuid_function_name
    }
}
