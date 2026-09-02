//! Submodule defining a struct providing options for the translation.

use alloc::borrow::Cow;
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

use sql_traits::structs::{ColumnDefinition, ColumnDefinitionScope, ColumnScope, ParserDB};

use crate::traits::{
    ArrayRepresentation, SessionVariableMapping, SessionVariablePattern, UuidRepresentation,
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
    /// The name of the SQLite UDF to use for ILIKE case folding. When Some,
    /// both sides of an ILIKE expression are wrapped in this function rather
    /// than `lower()`. This enables Unicode-aware case folding for non-ASCII
    /// patterns. Set via `with_ilike_fold_function`.
    ilike_fold_function: Option<String>,
}

#[cfg(feature = "arbitrary")]
impl<'a> arbitrary::Arbitrary<'a> for Pg2SqliteOptions {
    /// Randomises user-facing configuration only.
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
            user_defined_functions: Vec::new(),
            ilike_fold_function: None,
        }
    }
}

/// The script-wide facts a pass before translation collects, shared by every
/// [`TranslationContext`] derived for a query so a child costs no copying.
#[derive(Default, Clone)]
pub(crate) struct PrewalkCatalogs {
    spatial_indexes: Vec<(String, String)>,
    fts_indexes: Vec<(String, String)>,
    declared_object_names: Vec<String>,
    trigger_function_names: Vec<String>,
}

#[derive(Clone, Copy)]
enum TranslationScope<'a> {
    Query { scope: &'a ColumnScope<'a, 'a, ParserDB>, pseudo_row: bool },
    Definition(ColumnDefinitionScope<'a, 'a, 'a, ParserDB>),
}

/// The options a translation runs under, together with what it has learned
/// about the statements around the one it is translating.
///
/// Built from [`Pg2SqliteOptions`], which it dereferences to, so every setting
/// reads through it unchanged. Both directions carry it, because both resolve a
/// column reference against the relations in scope before reading a declared
/// type.
pub struct TranslationContext<'a> {
    options: Cow<'a, Pg2SqliteOptions>,
    catalogs: Cow<'a, PrewalkCatalogs>,
    /// The relations whose columns a reference in this context can name. A
    /// query attaches its own `FROM` clause and a definition attaches the table
    /// it belongs to, so a type-dependent rewrite reads the column it actually
    /// names rather than any column of that name in the schema. `None` means
    /// nothing is in scope, which refuses rather than guessing.
    scope: Option<TranslationScope<'a>>,
    /// The context this one was derived from, whose scope answers a reference
    /// the inner one cannot. PostgreSQL resolves an outer reference in a
    /// correlated subquery against the enclosing query, and a trigger body
    /// reads the guarded table the same way, so resolution walks outward rather
    /// than stopping at the innermost `FROM`.
    outer: Option<&'a TranslationContext<'a>>,
    /// The `WITH` clause of the query being translated, so a scope built for
    /// one arm of a set operation still reads a CTE reference as opaque rather
    /// than matching a base table of the same name.
    with: Option<&'a sqlparser::ast::With>,
    /// Names bound as PL/pgSQL variables in the body being translated. A
    /// variable is not a column, so a reference naming one has no declared type
    /// to read and is neither resolved nor refused.
    variables: &'a [String],
    /// Whether every relation available to the input is present in the schema.
    schema_is_complete: bool,
}

impl<'a> TranslationContext<'a> {
    /// Builds a context that carries `options` and no scope, which is what a
    /// caller starting a translation has.
    #[must_use]
    pub fn new(options: &'a Pg2SqliteOptions) -> Self {
        Self {
            options: Cow::Borrowed(options),
            catalogs: Cow::Owned(PrewalkCatalogs::default()),
            scope: None,
            outer: None,
            with: None,
            variables: &[],
            schema_is_complete: false,
        }
    }
    pub(crate) fn with_complete_schema(options: &'a Pg2SqliteOptions) -> Self {
        let mut context = Self::new(options);
        context.schema_is_complete = true;
        context
    }

    #[cfg(test)]
    pub(crate) fn from_owned(options: Pg2SqliteOptions) -> TranslationContext<'static> {
        TranslationContext {
            options: Cow::Owned(options),
            catalogs: Cow::Owned(PrewalkCatalogs::default()),
            scope: None,
            outer: None,
            with: None,
            variables: &[],
            schema_is_complete: false,
        }
    }

    /// The same context with `scope` attached, for translating the expressions
    /// of one query, one definition, or one naked expression a caller hands in.
    ///
    /// The script-wide catalogs are borrowed rather than copied, so a child is
    /// two pointers. Building one is the only way a scope enters translation,
    /// which is why forgetting to build one loses the scope rather than leaving
    /// an enclosing query's relations in place. A reference the attached scope
    /// cannot answer is tried against the enclosing one, as PostgreSQL resolves
    /// an outer reference.
    #[must_use]
    pub fn with_scope<'s>(
        &'s self,
        scope: &'s ColumnScope<'s, 's, ParserDB>,
    ) -> TranslationContext<'s> {
        TranslationContext {
            options: Cow::Borrowed(self.options.as_ref()),
            catalogs: Cow::Borrowed(self.catalogs.as_ref()),
            scope: Some(TranslationScope::Query { scope, pseudo_row: false }),
            outer: Some(self),
            with: self.with,
            variables: self.variables,
            schema_is_complete: self.schema_is_complete,
        }
    }
    pub(crate) fn with_pseudo_row_scope<'s>(
        &'s self,
        scope: &'s ColumnScope<'s, 's, ParserDB>,
    ) -> TranslationContext<'s> {
        TranslationContext {
            options: Cow::Borrowed(self.options.as_ref()),
            catalogs: Cow::Borrowed(self.catalogs.as_ref()),
            scope: Some(TranslationScope::Query { scope, pseudo_row: true }),
            outer: Some(self),
            with: self.with,
            variables: self.variables,
            schema_is_complete: self.schema_is_complete,
        }
    }

    pub(crate) fn with_definition_scope<'s>(
        &'s self,
        scope: ColumnDefinitionScope<'s, 's, 's, ParserDB>,
    ) -> TranslationContext<'s> {
        TranslationContext {
            options: Cow::Borrowed(self.options.as_ref()),
            catalogs: Cow::Borrowed(self.catalogs.as_ref()),
            scope: Some(TranslationScope::Definition(scope)),
            outer: Some(self),
            with: self.with,
            variables: self.variables,
            schema_is_complete: self.schema_is_complete,
        }
    }

    /// The same context noting `with` as the enclosing query's CTE clause.
    pub(crate) fn with_cte_clause<'s>(
        &'s self,
        with: Option<&'s sqlparser::ast::With>,
    ) -> TranslationContext<'s> {
        TranslationContext {
            options: Cow::Borrowed(self.options.as_ref()),
            catalogs: Cow::Borrowed(self.catalogs.as_ref()),
            scope: self.scope,
            outer: self.outer,
            with,
            variables: self.variables,
            schema_is_complete: self.schema_is_complete,
        }
    }

    /// The same context noting the PL/pgSQL variables in scope.
    pub(crate) fn with_variables<'s>(&'s self, variables: &'s [String]) -> TranslationContext<'s> {
        TranslationContext {
            options: Cow::Borrowed(self.options.as_ref()),
            catalogs: Cow::Borrowed(self.catalogs.as_ref()),
            scope: self.scope,
            outer: self.outer,
            with: self.with,
            variables,
            schema_is_complete: self.schema_is_complete,
        }
    }

    /// True when `name` is a PL/pgSQL variable rather than a column.
    #[must_use]
    pub(crate) fn is_variable(&self, name: &str) -> bool {
        self.variables.iter().any(|variable| variable == name)
    }

    #[must_use]
    pub(crate) const fn schema_is_complete(&self) -> bool {
        self.schema_is_complete
    }

    /// The enclosing query's `WITH` clause, if it has one.
    #[must_use]
    pub(crate) fn cte_clause(&self) -> Option<&sqlparser::ast::With> {
        self.with
    }
    /// The scopes a reference may resolve against, innermost first.
    pub(crate) fn column_definitions<'s>(
        &'s self,
        reference: &'s sqlparser::ast::Expr,
        pseudo_row_reference: Option<&'s sqlparser::ast::Expr>,
    ) -> impl Iterator<
        Item = Result<
            Option<ColumnDefinition<'s, 's, 's, ParserDB>>,
            sql_traits::errors::LookupError,
        >,
    > + 's {
        let mut context = Some(self);
        core::iter::from_fn(move || {
            while let Some(current) = context {
                context = current.outer;
                if let Some(scope) = current.scope {
                    return Some(match scope {
                        TranslationScope::Query { scope, pseudo_row } => {
                            scope.resolve_column_definition(
                                pseudo_row_reference.filter(|_| pseudo_row).unwrap_or(reference),
                            )
                        }
                        TranslationScope::Definition(scope) => {
                            scope.resolve_column_definition(reference)
                        }
                    });
                }
            }
            None
        })
    }

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
        if !self.catalogs.spatial_indexes.contains(&entry) {
            self.catalogs.to_mut().spatial_indexes.push(entry);
        }
    }

    /// Returns whether the given `(table, column)` pair was registered as a
    /// spatial index in the same translation unit. Case-insensitive on both
    /// inputs.
    #[must_use]
    pub(crate) fn has_spatial_index(&self, table: &str, column: &str) -> bool {
        let table = table.to_ascii_lowercase();
        let column = column.to_ascii_lowercase();
        self.catalogs.spatial_indexes.iter().any(|(t, c)| *t == table && *c == column)
    }

    /// Records `(table, column)` as having a translated FTS5 index. Same
    /// case-folded, idempotent shape as [`Self::add_spatial_index`].
    pub(crate) fn add_fts_index(&mut self, table: impl Into<String>, column: impl Into<String>) {
        let entry = (table.into().to_ascii_lowercase(), column.into().to_ascii_lowercase());
        if !self.catalogs.fts_indexes.contains(&entry) {
            self.catalogs.to_mut().fts_indexes.push(entry);
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
        self.catalogs.fts_indexes.iter().any(|(t, c)| *t == table && *c == column)
    }

    /// Records `name` as a declared SQLite object name, lowercased and
    /// idempotent so [`Self::has_declared_object_name`] matches
    /// case-insensitively as SQLite does.
    pub(crate) fn add_declared_object_name(&mut self, name: impl Into<String>) {
        let name = name.into().to_ascii_lowercase();
        if !self.catalogs.declared_object_names.contains(&name) {
            self.catalogs.to_mut().declared_object_names.push(name);
        }
    }

    /// Returns whether `name` collides with an object already declared in the
    /// translation unit. Case-insensitive, matching SQLite name resolution.
    #[must_use]
    pub(crate) fn has_declared_object_name(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        self.catalogs.declared_object_names.contains(&name)
    }

    /// Records `name` as a function executed by a trigger in the translation
    /// unit, lowercased and idempotent to match
    /// [`Self::has_trigger_function_name`].
    pub(crate) fn add_trigger_function_name(&mut self, name: impl Into<String>) {
        let name = name.into().to_ascii_lowercase();
        if !self.catalogs.trigger_function_names.contains(&name) {
            self.catalogs.to_mut().trigger_function_names.push(name);
        }
    }

    /// Returns whether a trigger in the translation unit executes `name`, in
    /// which case the function's body reaches the output inlined in that
    /// trigger and its definition is not lost.
    #[must_use]
    pub(crate) fn has_trigger_function_name(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        self.catalogs.trigger_function_names.contains(&name)
    }
}

impl core::ops::Deref for TranslationContext<'_> {
    type Target = Pg2SqliteOptions;

    fn deref(&self) -> &Self::Target {
        self.options.as_ref()
    }
}

impl Pg2SqliteOptions {
    #[must_use]
    /// Drops check constraints that call unsupported functions.
    pub fn with_remove_unsupported_check_constraints(mut self) -> Self {
        self.remove_unsupported_check_constraints = true;
        self
    }

    #[must_use]
    /// Returns whether unsupported check constraints are dropped.
    pub fn is_remove_unsupported_check_constraints_enabled(&self) -> bool {
        self.remove_unsupported_check_constraints
    }

    #[must_use]
    /// Sets how translated UUID columns are stored.
    pub fn with_uuid_representation(mut self, representation: UuidRepresentation) -> Self {
        self.uuid_representation = Some(representation);
        self
    }

    #[must_use]
    /// Returns the configured UUID storage representation.
    pub fn get_uuid_representation(&self) -> Option<UuidRepresentation> {
        self.uuid_representation
    }

    #[must_use]
    /// Sets the destination random UUID function, whose return type must match
    /// the storage representation.
    pub fn with_uuid_function_name(mut self, name: impl Into<String>) -> Self {
        self.uuid_function_name = name.into();
        self
    }

    #[must_use]
    /// Returns the destination random UUID function name.
    pub fn get_uuid_function_name(&self) -> &str {
        &self.uuid_function_name
    }

    #[must_use]
    /// Sets the destination version 7 UUID function.
    pub fn with_uuid_v7_function_name(mut self, name: impl Into<String>) -> Self {
        self.uuid_v7_function_name = Some(name.into());
        self
    }

    #[must_use]
    /// Returns the configured version 7 UUID function name.
    pub fn get_uuid_v7_function_name(&self) -> Option<&str> {
        self.uuid_v7_function_name.as_deref()
    }

    #[must_use]
    /// Sets the function that converts UUID text to a 16 byte blob.
    pub fn with_uuid_text_to_blob_function_name(mut self, name: impl Into<String>) -> Self {
        self.uuid_text_to_blob_function_name = Some(name.into());
        self
    }

    #[must_use]
    /// Returns the configured UUID text to blob function name.
    pub fn get_uuid_text_to_blob_function_name(&self) -> Option<&str> {
        self.uuid_text_to_blob_function_name.as_deref()
    }

    #[must_use]
    /// Sets how PostgreSQL arrays are represented in SQLite.
    pub fn with_array_representation(mut self, representation: ArrayRepresentation) -> Self {
        self.array_representation = Some(representation);
        self
    }

    #[must_use]
    /// Returns the configured array representation.
    pub fn get_array_representation(&self) -> Option<ArrayRepresentation> {
        self.array_representation
    }

    #[must_use]
    /// Sets the suffix used for RLS backing tables.
    pub fn with_rls_table_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.rls_table_suffix = suffix.into();
        self
    }

    #[must_use]
    /// Returns the suffix used for RLS backing tables.
    pub fn get_rls_table_suffix(&self) -> &str {
        &self.rls_table_suffix
    }

    #[must_use]
    /// Sets the marker used in read only deny trigger names.
    pub fn with_readonly_deny_trigger_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.readonly_deny_trigger_suffix = suffix.into();
        self
    }

    #[must_use]
    /// Returns the marker used in read only deny trigger names.
    pub fn get_readonly_deny_trigger_suffix(&self) -> &str {
        &self.readonly_deny_trigger_suffix
    }

    #[must_use]
    /// Filters grants and policies for the named session role.
    pub fn with_session_user_role(mut self, role: impl Into<String>) -> Self {
        self.session_user_role = Some(role.into());
        self
    }

    #[must_use]
    /// Returns the role used to filter grants and policies.
    pub fn get_session_user_role(&self) -> Option<&str> {
        self.session_user_role.as_deref()
    }

    #[must_use]
    /// Adds a PostgreSQL session variable mapping.
    pub fn with_session_variable(mut self, mapping: SessionVariableMapping) -> Self {
        self.session_variables.push(mapping);
        self
    }

    #[must_use]
    /// Returns the configured session variable mappings.
    pub fn get_session_variables(&self) -> &[SessionVariableMapping] {
        &self.session_variables
    }

    #[must_use]
    /// Returns the last mapping for the requested session variable pattern.
    pub fn find_session_variable(
        &self,
        pattern: &SessionVariablePattern,
    ) -> Option<&SessionVariableMapping> {
        self.session_variables.iter().rev().find(|m| &m.pg_pattern == pattern)
    }

    #[must_use]
    /// Maps current user and one current setting to the same SQLite function.
    pub fn with_session_user(
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

    #[must_use]
    /// Sets the table that records RLS validation results.
    pub fn with_rls_audit_table_name(mut self, name: impl Into<String>) -> Self {
        self.rls_audit_table_name = Some(name.into());
        self
    }

    #[must_use]
    /// Returns the configured RLS audit table name.
    pub fn get_rls_audit_table_name(&self) -> Option<&str> {
        self.rls_audit_table_name.as_deref()
    }

    #[must_use]
    /// Sets the boolean function that exempts generated write guards.
    pub fn with_write_exemption_function(mut self, name: impl Into<String>) -> Self {
        self.write_exemption_function = Some(name.into());
        self
    }

    #[must_use]
    /// Returns the configured write exemption function name.
    pub fn get_write_exemption_function(&self) -> Option<&str> {
        self.write_exemption_function.as_deref()
    }

    #[must_use]
    /// Makes RLS validation errors fatal and redirects supported returning
    /// inserts.
    pub fn with_strict_rls_validation(mut self) -> Self {
        self.strict_rls_validation = true;
        self
    }

    #[must_use]
    /// Returns whether strict RLS validation is enabled.
    pub fn is_strict_rls_validation(&self) -> bool {
        self.strict_rls_validation
    }

    #[must_use]
    /// Makes policy denied updates and deletes raise instead of affecting no
    /// rows.
    pub fn with_strict_rls_write_deny(mut self) -> Self {
        self.strict_rls_write_deny = true;
        self
    }

    #[must_use]
    /// Returns whether policy denied updates and deletes raise.
    pub fn is_strict_rls_write_deny(&self) -> bool {
        self.strict_rls_write_deny
    }

    #[must_use]
    /// Enables functions supplied by the SQLiteGIS extension.
    pub fn with_sqlitegis_enabled(mut self) -> Self {
        self.sqlitegis_enabled = true;
        self
    }

    #[must_use]
    /// Returns whether SQLiteGIS function translation is enabled.
    pub fn is_sqlitegis_enabled(&self) -> bool {
        self.sqlitegis_enabled
    }

    #[must_use]
    /// Declares that the destination provides the optional SQLite math
    /// functions.
    pub fn with_math_functions_available(mut self) -> Self {
        self.math_functions_available = true;
        self
    }

    #[must_use]
    /// Returns whether the destination provides SQLite math functions.
    pub fn is_math_functions_available(&self) -> bool {
        self.math_functions_available
    }

    #[must_use]
    /// Declares function names that the destination provides.
    pub fn with_user_defined_functions<S: Into<String>>(
        mut self,
        names: impl IntoIterator<Item = S>,
    ) -> Self {
        self.user_defined_functions
            .extend(names.into_iter().map(|name| name.into().to_ascii_lowercase()));
        self
    }

    #[must_use]
    /// Returns whether the destination declares the function name.
    pub fn declares_user_defined_function(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        self.user_defined_functions.contains(&name)
    }

    #[must_use]
    /// Sets the Unicode aware case folding function used for ILIKE.
    pub fn with_ilike_fold_function(mut self, name: impl Into<String>) -> Self {
        self.ilike_fold_function = Some(name.into());
        self
    }

    #[must_use]
    /// Returns the configured ILIKE case folding function name.
    pub fn get_ilike_fold_function(&self) -> Option<&str> {
        self.ilike_fold_function.as_deref()
    }
}
