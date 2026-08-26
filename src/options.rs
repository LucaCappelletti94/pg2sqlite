//! Submodule defining a struct providing options for the translation.

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

use crate::traits::{
    ArrayRepresentation, SessionVariableMapping, SessionVariablePattern, TranslationOptions,
    UuidRepresentation,
};

/// Struct to hold options for the translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pg2SqliteOptions {
    /// Whether to drop check constraints containing unsupported functions.
    remove_unsupported_check_constraints: bool,
    /// The representation of UUIDs in `SQLite`.
    uuid_representation: Option<UuidRepresentation>,
    /// The name of the function to use for random UUID generation.
    /// Its runtime return type must match `uuid_representation`.
    uuid_function_name: String,
    /// The name of the destination's version 7 UUID generator. `None` means
    /// the destination has none, and `uuidv7()` is refused rather than
    /// answered with a random value. Set via `with_uuid_v7_function_name`.
    uuid_v7_function_name: Option<String>,
    /// Optional UDF name for converting text UUID literals to 16-byte
    /// BLOBs at INSERT/UPDATE time. `None` means "emit
    /// `unhex(replace(literal, '-', ''))` inline" (pure SQLite, no UDF
    /// setup). Set via `with_uuid_text_to_blob_function_name`.
    uuid_text_to_blob_function_name: Option<String>,
    /// Names of host-registered functions the destination SQLite provides,
    /// stored lower-cased because SQLite resolves function names without
    /// regard to case. A name here passes through instead of being refused as
    /// unrecognised. Set via `with_user_defined_functions`.
    user_defined_functions: Vec<String>,
    /// The representation of PostgreSQL arrays in `SQLite`. `None` rejects
    /// every array construct instead of downgrading it silently.
    array_representation: Option<ArrayRepresentation>,
    /// The suffix to append to table names when renaming them for RLS views.
    rls_table_suffix: String,
    /// The reserved marker used to name deny triggers for read-only non-RLS
    /// tables (`<table><marker>_insert` and so on).
    readonly_deny_trigger_suffix: String,
    /// The role name to use when filtering policies.
    session_user_role: Option<String>,
    /// Mappings from PostgreSQL session variable patterns to SQLite functions.
    session_variables: Vec<SessionVariableMapping>,
    /// The name of the audit table for RLS validation monitoring.
    rls_audit_table_name: Option<String>,
    /// The caller-registered function that exempts generated write guards.
    write_exemption_function: Option<String>,
    /// Whether to enable strict RLS validation (abort on violations).
    strict_rls_validation: bool,
    /// Whether a write denied by a policy `USING` clause raises instead of
    /// affecting zero rows as PostgreSQL does.
    strict_rls_write_deny: bool,
    /// Whether to enable SQLiteGIS-backed PostGIS translation (passthrough of
    /// `ST_*` scalar functions in SQLiteGIS's catalog).
    sqlitegis_enabled: bool,
    /// Whether the destination SQLite is declared to provide the math
    /// functions, which ship only under `SQLITE_ENABLE_MATH_FUNCTIONS`.
    math_functions_available: bool,
    /// Spatially-indexed `(table, column)` pairs, both lowercased, populated
    /// automatically by `Pg2Sqlite::translate` from `CREATE INDEX ... USING
    /// gist (...)` statements in the input. Consumed by the query-time
    /// predicate rewriting pass to drive `ST_*` predicates through the rtree
    /// shadow table. Intentionally not exposed in the public builder API:
    /// the catalog is derived from translation context, not user config.
    pub(crate) spatial_indexes: Vec<(String, String)>,
    /// FTS5-indexed `(table, column)` pairs, both lowercased, populated
    /// automatically by `Pg2Sqlite::translate` from `CREATE INDEX ... USING
    /// GIN (to_tsvector(...))` (or GiST equivalent) statements in the input.
    /// Consumed by the query-time `@@ to_tsquery(...)` rewrite to validate
    /// that the matched column has a declared FTS5 index before emitting
    /// the IN-subquery against `<table>_fts`. Without this gate the
    /// rewrite produced SQL that runtime-errored with "no such table".
    /// Intentionally not exposed in the public builder API: the catalog
    /// is derived from translation context, not user config.
    pub(crate) fts_indexes: Vec<(String, String)>,
    /// SQLite-unqualified names of every table, index, trigger, and view
    /// declared in the translation unit, lowercased. Prewalked from the input
    /// statements by `Pg2Sqlite::translate` and consumed by the read-only
    /// deny-trigger pass to reject a generated trigger name that would collide
    /// with an existing object. The translation schema omits index and trigger
    /// definitions, so this catalog is the authoritative name source.
    /// Intentionally not exposed in the public builder API: it is derived from
    /// translation context, not user config.
    pub(crate) declared_object_names: Vec<String>,
    /// Names of the functions that a `CREATE TRIGGER` in the translation unit
    /// executes, lowercased and unqualified. Prewalked from the input
    /// statements by `Pg2Sqlite::translate` for the same reason as
    /// `declared_object_names`: the translation schema omits trigger
    /// definitions, so nothing else knows which functions a trigger inlines.
    /// Consumed by the `CREATE FUNCTION` arm, which stays silent for a function
    /// a trigger realises and warns for one that is genuinely dropped.
    /// Intentionally not exposed in the public builder API: it is derived from
    /// translation context, not user config.
    pub(crate) trigger_function_names: Vec<String>,
    /// The name of the SQLite UDF to use for ILIKE case folding. When Some,
    /// both sides of an ILIKE expression are wrapped in this function rather
    /// than `lower()`. This enables Unicode-aware case folding for non-ASCII
    /// patterns. Set via `with_ilike_fold_function`.
    ilike_fold_function: Option<String>,
}

#[cfg(feature = "arbitrary")]
impl<'a> arbitrary::Arbitrary<'a> for Pg2SqliteOptions {
    /// Randomises only the user-facing configuration fields. The
    /// `spatial_indexes` and `fts_indexes` catalogs are populated by
    /// the translator from `CREATE INDEX` statements in the input and
    /// must start empty - randomising them would feed the translator
    /// bogus catalog state that no real caller could produce.
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self {
            remove_unsupported_check_constraints: bool::arbitrary(u)?,
            uuid_representation: Option::<UuidRepresentation>::arbitrary(u)?,
            uuid_function_name: String::arbitrary(u)?,
            uuid_v7_function_name: Option::<String>::arbitrary(u)?,
            uuid_text_to_blob_function_name: Option::<String>::arbitrary(u)?,
            array_representation: Option::<ArrayRepresentation>::arbitrary(u)?,
            rls_table_suffix: String::arbitrary(u)?,
            readonly_deny_trigger_suffix: String::arbitrary(u)?,
            session_user_role: Option::<String>::arbitrary(u)?,
            session_variables: Vec::<SessionVariableMapping>::arbitrary(u)?,
            rls_audit_table_name: Option::<String>::arbitrary(u)?,
            write_exemption_function: Option::<String>::arbitrary(u)?,
            strict_rls_validation: bool::arbitrary(u)?,
            strict_rls_write_deny: bool::arbitrary(u)?,
            sqlitegis_enabled: bool::arbitrary(u)?,
            math_functions_available: bool::arbitrary(u)?,
            spatial_indexes: Vec::new(),
            fts_indexes: Vec::new(),
            declared_object_names: Vec::new(),
            trigger_function_names: Vec::new(),
            user_defined_functions: Vec::<String>::arbitrary(u)?,
            ilike_fold_function: Option::<String>::arbitrary(u)?,
        })
    }
}

impl Default for Pg2SqliteOptions {
    fn default() -> Self {
        Self {
            remove_unsupported_check_constraints: false,
            uuid_representation: None,
            uuid_function_name: "uuid".to_string(),
            uuid_v7_function_name: None,
            uuid_text_to_blob_function_name: None,
            array_representation: None,
            rls_table_suffix: "_rls".to_string(),
            readonly_deny_trigger_suffix: "__readonly".to_string(),
            session_user_role: None,
            session_variables: Vec::new(),
            rls_audit_table_name: None,
            write_exemption_function: None,
            strict_rls_validation: false,
            strict_rls_write_deny: false,
            sqlitegis_enabled: false,
            math_functions_available: false,
            spatial_indexes: Vec::new(),
            fts_indexes: Vec::new(),
            declared_object_names: Vec::new(),
            trigger_function_names: Vec::new(),
            user_defined_functions: Vec::new(),
            ilike_fold_function: None,
        }
    }
}

impl Pg2SqliteOptions {
    /// Records `(table, column)` as having a translated spatial index. Both
    /// are lowercased on insert so later lookups via
    /// [`Self::has_spatial_index`] can match the parser's case-folded
    /// identifiers without re-normalizing. Idempotent: duplicate entries
    /// are filtered out.
    pub(crate) fn add_spatial_index(
        &mut self,
        table: impl Into<String>,
        column: impl Into<String>,
    ) {
        let entry = (table.into().to_ascii_lowercase(), column.into().to_ascii_lowercase());
        if !self.spatial_indexes.contains(&entry) {
            self.spatial_indexes.push(entry);
        }
    }

    /// Returns whether the given `(table, column)` pair was registered as a
    /// spatial index in the same translation unit. Case-insensitive on both
    /// inputs.
    #[must_use]
    pub(crate) fn has_spatial_index(&self, table: &str, column: &str) -> bool {
        let table = table.to_ascii_lowercase();
        let column = column.to_ascii_lowercase();
        self.spatial_indexes.iter().any(|(t, c)| *t == table && *c == column)
    }

    /// Records `(table, column)` as having a translated FTS5 index. Same
    /// case-folded, idempotent shape as [`Self::add_spatial_index`].
    pub(crate) fn add_fts_index(&mut self, table: impl Into<String>, column: impl Into<String>) {
        let entry = (table.into().to_ascii_lowercase(), column.into().to_ascii_lowercase());
        if !self.fts_indexes.contains(&entry) {
            self.fts_indexes.push(entry);
        }
    }

    /// Returns whether the given `(table, column)` pair was registered as an
    /// FTS5 index in the same translation unit. Case-insensitive on both
    /// inputs. Drives the `to_tsvector(...) @@ to_tsquery(...)` rewrite gate
    /// in `translate_fts_expression`.
    #[must_use]
    pub(crate) fn has_fts_index(&self, table: &str, column: &str) -> bool {
        let table = table.to_ascii_lowercase();
        let column = column.to_ascii_lowercase();
        self.fts_indexes.iter().any(|(t, c)| *t == table && *c == column)
    }

    /// Records `name` as a declared SQLite object name, lowercased and
    /// idempotent so [`Self::has_declared_object_name`] matches
    /// case-insensitively as SQLite does.
    pub(crate) fn add_declared_object_name(&mut self, name: impl Into<String>) {
        let name = name.into().to_ascii_lowercase();
        if !self.declared_object_names.contains(&name) {
            self.declared_object_names.push(name);
        }
    }

    /// Returns whether `name` collides with an object already declared in the
    /// translation unit. Case-insensitive, matching SQLite name resolution.
    #[must_use]
    pub(crate) fn has_declared_object_name(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        self.declared_object_names.contains(&name)
    }

    /// Records `name` as a function executed by a trigger in the translation
    /// unit, lowercased and idempotent to match
    /// [`Self::has_trigger_function_name`].
    pub(crate) fn add_trigger_function_name(&mut self, name: impl Into<String>) {
        let name = name.into().to_ascii_lowercase();
        if !self.trigger_function_names.contains(&name) {
            self.trigger_function_names.push(name);
        }
    }

    /// Returns whether a trigger in the translation unit executes `name`, in
    /// which case the function's body reaches the output inlined in that
    /// trigger and its definition is not lost.
    #[must_use]
    pub(crate) fn has_trigger_function_name(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        self.trigger_function_names.contains(&name)
    }
}

impl TranslationOptions for Pg2SqliteOptions {
    fn with_remove_unsupported_check_constraints(mut self) -> Self {
        self.remove_unsupported_check_constraints = true;
        self
    }

    fn is_remove_unsupported_check_constraints_enabled(&self) -> bool {
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

    fn with_uuid_v7_function_name(mut self, name: impl Into<String>) -> Self {
        self.uuid_v7_function_name = Some(name.into());
        self
    }

    fn get_uuid_v7_function_name(&self) -> Option<&str> {
        self.uuid_v7_function_name.as_deref()
    }

    fn with_uuid_text_to_blob_function_name(mut self, name: impl Into<String>) -> Self {
        self.uuid_text_to_blob_function_name = Some(name.into());
        self
    }

    fn get_uuid_text_to_blob_function_name(&self) -> Option<&str> {
        self.uuid_text_to_blob_function_name.as_deref()
    }

    fn with_array_representation(mut self, representation: ArrayRepresentation) -> Self {
        self.array_representation = Some(representation);
        self
    }

    fn get_array_representation(&self) -> Option<ArrayRepresentation> {
        self.array_representation
    }

    fn with_rls_table_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.rls_table_suffix = suffix.into();
        self
    }

    fn get_rls_table_suffix(&self) -> &str {
        &self.rls_table_suffix
    }

    fn with_readonly_deny_trigger_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.readonly_deny_trigger_suffix = suffix.into();
        self
    }

    fn get_readonly_deny_trigger_suffix(&self) -> &str {
        &self.readonly_deny_trigger_suffix
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

    fn find_session_variable(
        &self,
        pattern: &SessionVariablePattern,
    ) -> Option<&SessionVariableMapping> {
        self.session_variables.iter().rev().find(|m| &m.pg_pattern == pattern)
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

    fn with_rls_audit_table_name(mut self, name: impl Into<String>) -> Self {
        self.rls_audit_table_name = Some(name.into());
        self
    }

    fn get_rls_audit_table_name(&self) -> Option<&str> {
        self.rls_audit_table_name.as_deref()
    }

    fn with_write_exemption_function(mut self, name: impl Into<String>) -> Self {
        self.write_exemption_function = Some(name.into());
        self
    }

    fn get_write_exemption_function(&self) -> Option<&str> {
        self.write_exemption_function.as_deref()
    }

    fn with_strict_rls_validation(mut self) -> Self {
        self.strict_rls_validation = true;
        self
    }

    fn is_strict_rls_validation(&self) -> bool {
        self.strict_rls_validation
    }

    fn with_strict_rls_write_deny(mut self) -> Self {
        self.strict_rls_write_deny = true;
        self
    }

    fn is_strict_rls_write_deny(&self) -> bool {
        self.strict_rls_write_deny
    }

    fn with_sqlitegis_enabled(mut self) -> Self {
        self.sqlitegis_enabled = true;
        self
    }

    fn is_sqlitegis_enabled(&self) -> bool {
        self.sqlitegis_enabled
    }

    fn with_math_functions_available(mut self) -> Self {
        self.math_functions_available = true;
        self
    }

    fn is_math_functions_available(&self) -> bool {
        self.math_functions_available
    }

    fn with_user_defined_functions<S: Into<String>>(
        mut self,
        names: impl IntoIterator<Item = S>,
    ) -> Self {
        self.user_defined_functions
            .extend(names.into_iter().map(|name| name.into().to_ascii_lowercase()));
        self
    }

    fn declares_user_defined_function(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        self.user_defined_functions.contains(&name)
    }

    fn with_ilike_fold_function(mut self, name: impl Into<String>) -> Self {
        self.ilike_fold_function = Some(name.into());
        self
    }

    fn get_ilike_fold_function(&self) -> Option<&str> {
        self.ilike_fold_function.as_deref()
    }
}
