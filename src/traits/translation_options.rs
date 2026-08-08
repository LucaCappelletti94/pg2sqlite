//! Submodule defining a set of translation options.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

/// Enum for defining the representation of UUIDs in `SQLite`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum UuidRepresentation {
    /// Represent UUIDs as BLOBs.
    Blob,
    /// Represent UUIDs as TEXT.
    Text,
}

/// Enum for defining the representation of PostgreSQL arrays in `SQLite`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum ArrayRepresentation {
    /// Store arrays as JSON array text in a `TEXT` column and translate array
    /// operations through the SQLite `json1` extension, which is compiled in
    /// by default since SQLite 3.38.
    ///
    /// The stored form is a JSON array (`[1,2,3]`), not PostgreSQL's own array
    /// output format (`{1,2,3}`), so a pipeline copying rows between the two
    /// databases has to convert values as well as schema.
    Json,
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
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum SessionVariablePattern {
    /// Matches `current_user` in PostgreSQL.
    CurrentUser,
    /// Matches `current_setting('variable_name')` in PostgreSQL.
    CurrentSetting {
        /// The name of the session variable (e.g., "app.user_id").
        name: String,
    },
}

impl core::fmt::Display for SessionVariablePattern {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::CurrentUser => write!(f, "current_user"),
            Self::CurrentSetting { name } => write!(f, "current_setting('{name}')"),
        }
    }
}

/// A mapping from a PostgreSQL session variable pattern to a SQLite function.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
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

/// Trait defining translation options for the library.
pub trait TranslationOptions {
    #[must_use]
    /// Sets the option to drop check constraints containing unsupported
    fn remove_unsupported_check_constraints(self) -> Self;

    /// Returns whether to drop check constraints containing unsupported
    /// functions.
    fn should_remove_unsupported_check_constraints(&self) -> bool;

    #[must_use]
    /// Sets the UUID storage representation for translated SQLite schemas.
    ///
    /// This must be compatible with the runtime return type of the configured
    /// UUID function name (`with_uuid_function_name`).
    fn with_uuid_representation(self, representation: UuidRepresentation) -> Self;

    /// Returns the representation of UUIDs in `SQLite`.
    fn get_uuid_representation(&self) -> Option<UuidRepresentation>;

    #[must_use]
    /// Sets the UUID generation function name; the runtime return type must
    /// match the configured UUID representation.
    fn with_uuid_function_name(self, name: impl Into<String>) -> Self;

    /// Returns the name of the function to use for UUID generation.
    fn get_uuid_function_name(&self) -> &str;

    #[must_use]
    /// Sets a UDF name for converting text UUID literals to 16-byte BLOBs at
    /// INSERT and UPDATE time (for [`UuidRepresentation::Blob`]). If unset,
    /// the translator emits `unhex(replace(literal, '-', ''))` inline.
    fn with_uuid_text_to_blob_function_name(self, name: impl Into<String>) -> Self;

    /// Returns the configured UUID text-to-blob UDF name, if set.
    fn get_uuid_text_to_blob_function_name(&self) -> Option<&str>;

    #[must_use]
    /// Sets the array representation; unset by default, which rejects all array
    /// constructs instead of silently downgrading.
    ///
    /// Under [`ArrayRepresentation::Json`] a subscript such as `tags[1]` is
    /// read as a one-based *array* subscript. PostgreSQL also allows
    /// zero-based subscripts on `jsonb` values, and the translator cannot tell
    /// the two apart from the expression alone, so write `payload -> 0`
    /// instead of `payload[0]` for JSON documents.
    fn with_array_representation(self, representation: ArrayRepresentation) -> Self;

    /// Returns the representation of PostgreSQL arrays in `SQLite`.
    fn get_array_representation(&self) -> Option<ArrayRepresentation>;

    #[must_use]
    /// Sets the suffix to append to table names when renaming them for RLS
    /// views. Default is "_rls".
    fn with_rls_table_suffix(self, suffix: impl Into<String>) -> Self;

    /// Returns the suffix used for renamed RLS tables.
    fn get_rls_table_suffix(&self) -> &str;

    #[must_use]
    /// Sets the reserved marker used to name the deny triggers emitted for a
    /// read-only non-RLS table. Deny triggers are named
    /// `<table><marker>_insert` / `_update` / `_delete`. Default is
    /// `"__readonly"`. Change it if the default collides with an object your
    /// schema already defines.
    fn with_readonly_deny_trigger_suffix(self, suffix: impl Into<String>) -> Self;

    /// Returns the reserved marker used to name read-only deny triggers.
    fn get_readonly_deny_trigger_suffix(&self) -> &str;

    #[must_use]
    /// Filters translation to policies for PUBLIC or the named role, emitting
    /// each table per the role's grants (omitted, read-only, or writable).
    ///
    /// Apply contract: consumers applying authoritative changesets to a
    /// role-translated replica MUST disable triggers
    /// (`SQLITE_DBCONFIG_ENABLE_TRIGGER` off). A server patch replays
    /// statements whose triggers already ran server-side, so the deny
    /// triggers are for interactive statements only; applying with them
    /// enabled would abort patch delivery to the read-only tables that
    /// receive their data that way.
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

    #[must_use]
    /// Sets the name of the audit table for RLS validation monitoring.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::*;
    ///
    /// let options =
    ///     Pg2SqliteOptions::default().with_rls_audit_table_name("rls_violations".to_string());
    /// ```
    fn with_rls_audit_table_name(self, name: impl Into<String>) -> Self;

    /// Returns the configured RLS audit table name, if set.
    fn get_rls_audit_table_name(&self) -> Option<&str>;

    #[must_use]
    /// Enables passthrough of `ST_*` scalar functions via the SQLiteGIS SQLite
    /// extension; functions outside SQLiteGIS's catalog still error as
    /// unsupported. The caller must load the extension on the destination
    /// connection at runtime.
    fn with_sqlitegis_enabled(self) -> Self;

    /// Returns whether SQLiteGIS-backed PostGIS translation is enabled.
    fn is_sqlitegis_enabled(&self) -> bool;

    #[must_use]
    /// Declares that the destination SQLite provides the math functions
    /// (`sqrt`, `pow`, `ln`, `exp`, and friends). They ship only when SQLite is
    /// built with `SQLITE_ENABLE_MATH_FUNCTIONS`, so the translator assumes
    /// they are absent by default.
    ///
    /// With this off, `sqrt(x)` and the operators that lower onto the math
    /// functions (`^`, `|/`, `||/`) are rejected. With it on they translate,
    /// and the caller is responsible for the destination actually having the
    /// functions, whether from the build flag or a registered UDF.
    fn with_math_functions_available(self) -> Self;

    /// Returns whether the destination is declared to have the math functions.
    fn are_math_functions_available(&self) -> bool;

    #[must_use]
    /// Declares host-registered functions the destination SQLite provides.
    ///
    /// The translator refuses a function name it does not recognise, because
    /// emitting one produces SQL that fails at run time with `no such
    /// function`. A name declared here passes through instead. SQLite resolves
    /// function names without regard to case, so the declaration does too.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::*;
    ///
    /// let options = Pg2SqliteOptions::default().with_user_defined_functions(["levenshtein"]);
    /// assert!(options.declares_user_defined_function("LEVENSHTEIN"));
    /// assert!(!options.declares_user_defined_function("soundex"));
    /// ```
    fn with_user_defined_functions<S: Into<String>>(
        self,
        names: impl IntoIterator<Item = S>,
    ) -> Self;

    /// Returns whether `name` was declared through
    /// [`with_user_defined_functions`](TranslationOptions::with_user_defined_functions).
    fn declares_user_defined_function(&self, name: &str) -> bool;

    #[must_use]
    /// Enables strict RLS validation (abort on violation instead of logging
    /// only).
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::*;
    ///
    /// let options = Pg2SqliteOptions::default()
    ///     .with_rls_audit_table_name("rls_violations".to_string())
    ///     .with_strict_rls_validation(); // Abort on violations
    /// ```
    fn with_strict_rls_validation(self) -> Self;

    /// Returns whether strict RLS validation is enabled.
    fn is_strict_rls_validation(&self) -> bool;

    #[must_use]
    /// Raises instead of silently affecting zero rows when no policy makes a
    /// row updatable or deletable.
    ///
    /// The default matches PostgreSQL, which evaluates a policy's `USING`
    /// clause to decide which rows an `UPDATE` or `DELETE` can target and
    /// reports zero rows affected when none qualify. Verified against
    /// PostgreSQL 16: with row level security enabled and no applicable
    /// policy, `UPDATE` and `DELETE` return a zero count and no error.
    ///
    /// Enable this to surface a missing policy loudly during development. Note
    /// it diverges from PostgreSQL, so an application treating a zero-row
    /// write as ordinary will see an error where PostgreSQL succeeded.
    ///
    /// This does not affect `INSERT`, nor an `UPDATE` whose new row fails a
    /// `WITH CHECK` clause. PostgreSQL raises for both, so the translation
    /// always does too.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::*;
    ///
    /// let options = Pg2SqliteOptions::default().with_strict_rls_write_deny();
    /// assert!(options.is_strict_rls_write_deny());
    /// ```
    fn with_strict_rls_write_deny(self) -> Self;

    /// Returns whether a write denied by a policy's `USING` clause raises
    /// rather than affecting zero rows.
    fn is_strict_rls_write_deny(&self) -> bool;
}
