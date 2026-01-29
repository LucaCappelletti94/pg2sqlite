//! Submodule defining a set of translation options.

/// Enum for defining the representation of UUIDs in SQLite.
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

/// Trait defining translation options for the  library.
pub trait TranslationOptions {
    #[must_use]
    /// Sets the option to drop check constraints containing unsupported
    fn remove_unsupported_check_constraints(self) -> Self;

    /// Returns whether to drop check constraints containing unsupported
    /// functions.
    fn should_remove_unsupported_check_constraints(&self) -> bool;

    #[must_use]
    /// Sets the option to use pure SQL for UUID generation.
    fn use_pure_sql_for_uuid(self) -> Self;

    /// Returns whether to use pure SQL for UUID generation.
    fn should_use_pure_sql_for_uuid(&self) -> bool;

    #[must_use]
    /// Sets the option to specify the representation of UUIDs in SQLite.
    fn with_uuid_representation(self, representation: UuidRepresentation) -> Self;

    /// Returns the representation of UUIDs in SQLite.
    fn get_uuid_representation(&self) -> Option<UuidRepresentation>;

    #[must_use]
    /// Sets the option to specify the name of the function to use for UUID
    /// generation.
    fn with_uuid_function_name(self, name: String) -> Self;

    /// Returns the name of the function to use for UUID generation.
    fn get_uuid_function_name(&self) -> &str;
}
