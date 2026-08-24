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

use sqlparser::{
    ast::DataType,
    dialect::PostgreSqlDialect,
    parser::{Parser, ParserError},
    tokenizer::Token,
};

use crate::errors::Error;

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
    /// The PostgreSQL type the setting holds, when the caller recorded one.
    ///
    /// PostgreSQL's `current_setting` answers text, so a predicate comparing it
    /// against a `uuid` or an `integer` column casts it, and the cast is lost
    /// going to SQLite because the replica's function answers the value
    /// directly. Recording the type here lets the cast be written again on
    /// the way back.
    pub pg_type: Option<String>,
    /// Whether this mapping represents the tolerant `current_setting(name,
    /// true)` form rather than the strict one-argument
    /// `current_setting(name)` form.
    ///
    /// True: the reverse direction emits `current_setting(name, true)`
    /// (missing_ok), which answers NULL for an unset setting instead of
    /// raising. False: the reverse direction emits `current_setting(name)`,
    /// which raises when the setting is unset.
    pub missing_ok: bool,
}

impl SessionVariableMapping {
    /// Creates a new session variable mapping. Defaults to tolerant (missing_ok
    /// = true).
    #[must_use]
    pub fn new(pg_pattern: SessionVariablePattern, sqlite_function: impl Into<String>) -> Self {
        Self {
            pg_pattern,
            sqlite_function: sqlite_function.into(),
            pg_type: None,
            missing_ok: true,
        }
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

    /// Creates a strict mapping for `current_setting('name')`, which reverses
    /// to `current_setting(name)` with no second argument.
    ///
    /// The strict form raises when the setting is unset. The tolerant form
    /// `current_setting(name, true)` (the default from [`current_setting`])
    /// answers NULL instead of raising.
    ///
    /// [`current_setting`]: SessionVariableMapping::current_setting
    #[must_use]
    pub fn current_setting_strict(
        name: impl Into<String>,
        sqlite_function: impl Into<String>,
    ) -> Self {
        Self { missing_ok: false, ..Self::current_setting(name, sqlite_function) }
    }

    /// Records the PostgreSQL type the setting holds, spelled as PostgreSQL
    /// spells it.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::*;
    ///
    /// let mapping =
    ///     SessionVariableMapping::current_setting("app.user_id", "app_user_id").with_pg_type("uuid");
    /// assert_eq!(mapping.pg_type.as_deref(), Some("uuid"));
    /// ```
    #[must_use]
    pub fn with_pg_type(mut self, pg_type: impl Into<String>) -> Self {
        self.pg_type = Some(pg_type.into());
        self
    }

    /// The recorded type as a node, or `None` when the caller recorded none.
    ///
    /// A spelling that leaves input behind is refused rather than truncated,
    /// because `parse_data_type` reads one type and stops: `uuid oops` would
    /// otherwise become `uuid`, and `oops uuid` the custom type `oops`, which
    /// PostgreSQL refuses only once the emitted SQL runs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SessionVariableTypeUnreadable`] when the recorded
    /// spelling does not parse as a PostgreSQL type, or parses and leaves input
    /// behind.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::*;
    /// use sqlparser::ast::{DataType, ExactNumberInfo};
    ///
    /// let mapping = SessionVariableMapping::current_setting("app.rate", "app_rate")
    ///     .with_pg_type("numeric(10,2)");
    /// assert_eq!(
    ///     mapping.pg_type_node()?,
    ///     Some(DataType::Numeric(ExactNumberInfo::PrecisionAndScale(10, 2)))
    /// );
    /// # Ok::<(), pg2sqlite::errors::Error>(())
    /// ```
    pub fn pg_type_node(&self) -> Result<Option<DataType>, Error> {
        let Some(pg_type) = &self.pg_type else {
            return Ok(None);
        };
        let unreadable = |source: Option<ParserError>| {
            Error::SessionVariableTypeUnreadable {
                pattern: self.pg_pattern.to_string(),
                pg_type: pg_type.clone(),
                source,
            }
        };
        let mut parser = Parser::new(&PostgreSqlDialect {})
            .try_with_sql(pg_type)
            .map_err(|source| unreadable(Some(source)))?;
        let data_type = parser.parse_data_type().map_err(|source| unreadable(Some(source)))?;
        if parser.peek_token().token == Token::EOF {
            Ok(Some(data_type))
        } else {
            Err(unreadable(None))
        }
    }
}

/// Trait defining translation options for the library.
pub trait TranslationOptions {
    /// Sets the option to drop check constraints containing unsupported
    #[must_use]
    fn with_remove_unsupported_check_constraints(self) -> Self;

    /// Returns whether to drop check constraints containing unsupported
    /// functions.
    fn is_remove_unsupported_check_constraints_enabled(&self) -> bool;

    /// Sets the UUID storage representation for translated SQLite schemas.
    ///
    /// This must be compatible with the runtime return type of the configured
    /// UUID function name (`with_uuid_function_name`).
    #[must_use]
    fn with_uuid_representation(self, representation: UuidRepresentation) -> Self;

    /// Returns the representation of UUIDs in `SQLite`.
    fn get_uuid_representation(&self) -> Option<UuidRepresentation>;

    /// Sets the name of the destination's random-UUID generator, which is what
    /// `gen_random_uuid()`, `uuid_generate_v4()` and `uuidv4()` translate to.
    /// The runtime return type must match the configured UUID representation.
    ///
    /// This does not answer `uuidv7()`, whose value is ordered by creation
    /// time rather than random. That one has its own name, set through
    /// [`with_uuid_v7_function_name`](TranslationOptions::with_uuid_v7_function_name).
    #[must_use]
    fn with_uuid_function_name(self, name: impl Into<String>) -> Self;

    /// Returns the name of the function to use for random UUID generation.
    fn get_uuid_function_name(&self) -> &str;

    /// Sets the name of the destination's version 7 UUID generator, which is
    /// what `uuidv7()` translates to.
    ///
    /// Unset by default, and `uuidv7()` is then refused rather than answered
    /// with a random UUID, because the first 48 bits of a version 7 value are
    /// the millisecond it was created and a schema asking for one is usually
    /// buying that ordering. SQLite's bundled `uuid.c` has no version 7
    /// generator at all, and SQLean's `uuid` module calls its own `uuid7`, so
    /// the name is taken from the caller rather than assumed.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::*;
    ///
    /// let options = Pg2SqliteOptions::default().with_uuid_v7_function_name("uuid7");
    /// assert_eq!(options.get_uuid_v7_function_name(), Some("uuid7"));
    /// ```
    #[must_use]
    fn with_uuid_v7_function_name(self, name: impl Into<String>) -> Self;

    /// Returns the configured version 7 UUID generator name, if set.
    fn get_uuid_v7_function_name(&self) -> Option<&str>;

    /// Sets a UDF name for converting text UUID literals to 16-byte BLOBs at
    /// INSERT and UPDATE time (for [`UuidRepresentation::Blob`]). If unset,
    /// the translator emits `unhex(replace(literal, '-', ''))` inline.
    #[must_use]
    fn with_uuid_text_to_blob_function_name(self, name: impl Into<String>) -> Self;

    /// Returns the configured UUID text-to-blob UDF name, if set.
    fn get_uuid_text_to_blob_function_name(&self) -> Option<&str>;

    /// Sets the array representation; unset by default, which rejects all array
    /// constructs instead of silently downgrading.
    ///
    /// Under [`ArrayRepresentation::Json`] a subscript such as `tags[1]` is
    /// read as a one-based *array* subscript. PostgreSQL also allows
    /// zero-based subscripts on `jsonb` values, and the translator cannot tell
    /// the two apart from the expression alone, so write `payload -> 0`
    /// instead of `payload[0]` for JSON documents.
    #[must_use]
    fn with_array_representation(self, representation: ArrayRepresentation) -> Self;

    /// Returns the representation of PostgreSQL arrays in `SQLite`.
    fn get_array_representation(&self) -> Option<ArrayRepresentation>;

    /// Sets the suffix to append to table names when renaming them for RLS
    /// views. Default is "_rls".
    #[must_use]
    fn with_rls_table_suffix(self, suffix: impl Into<String>) -> Self;

    /// Returns the suffix used for renamed RLS tables.
    fn get_rls_table_suffix(&self) -> &str;

    /// Sets the reserved marker used to name the deny triggers emitted for a
    /// read-only non-RLS table. Deny triggers are named
    /// `<table><marker>_insert` / `_update` / `_delete`. Default is
    /// `"__readonly"`. Change it if the default collides with an object your
    /// schema already defines.
    #[must_use]
    fn with_readonly_deny_trigger_suffix(self, suffix: impl Into<String>) -> Self;

    /// Returns the reserved marker used to name read-only deny triggers.
    fn get_readonly_deny_trigger_suffix(&self) -> &str;

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
    #[must_use]
    fn with_session_user_role(self, role: impl Into<String>) -> Self;

    /// Returns the session user role for policy filtering.
    fn get_session_user_role(&self) -> Option<&str>;

    /// Adds a session variable mapping from a PostgreSQL pattern to a SQLite
    /// function.
    #[must_use]
    fn with_session_variable(self, mapping: SessionVariableMapping) -> Self;

    /// Returns all configured session variable mappings.
    fn get_session_variables(&self) -> &[SessionVariableMapping];

    /// Finds the mapping for a given PostgreSQL session variable pattern.
    /// Returns `None` if no mapping is configured.
    ///
    /// The last matching mapping wins, so a later call to
    /// [`with_session_variable`](TranslationOptions::with_session_variable)
    /// overrides an earlier one for the same pattern.
    fn find_session_variable(
        &self,
        pattern: &SessionVariablePattern,
    ) -> Option<&SessionVariableMapping>;

    /// Convenience method to set up a session user function that handles both
    /// `current_user` and `current_setting('variable_name')` patterns.
    #[must_use]
    fn with_session_user(
        self,
        variable_name: impl Into<String>,
        sqlite_function: impl Into<String>,
    ) -> Self;

    /// Sets the name of the audit table for RLS validation monitoring.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::*;
    ///
    /// let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_violations");
    /// ```
    #[must_use]
    fn with_rls_audit_table_name(self, name: impl Into<String>) -> Self;

    /// Returns the configured RLS audit table name, if set.
    fn get_rls_audit_table_name(&self) -> Option<&str>;

    /// Enables passthrough of `ST_*` scalar functions via the SQLiteGIS SQLite
    /// extension; functions outside SQLiteGIS's catalog still error as
    /// unsupported. The caller must load the extension on the destination
    /// connection at runtime.
    #[must_use]
    fn with_sqlitegis_enabled(self) -> Self;

    /// Returns whether SQLiteGIS-backed PostGIS translation is enabled.
    fn is_sqlitegis_enabled(&self) -> bool;

    /// Declares that the destination SQLite provides the math functions
    /// (`sqrt`, `pow`, `ln`, `exp`, and friends). They ship only when SQLite is
    /// built with `SQLITE_ENABLE_MATH_FUNCTIONS`, so the translator assumes
    /// they are absent by default.
    ///
    /// With this off, `sqrt(x)` and the operators that lower onto the math
    /// functions (`^`, `|/`, `||/`) are rejected. With it on they translate,
    /// and the caller is responsible for the destination actually having the
    /// functions, whether from the build flag or a registered UDF.
    #[must_use]
    fn with_math_functions_available(self) -> Self;

    /// Returns whether the destination is declared to have the math functions.
    fn is_math_functions_available(&self) -> bool;

    /// Declares functions the destination provides, whichever destination the
    /// translation is heading for.
    ///
    /// Both directions refuse a function name they do not recognise, because
    /// emitting one produces SQL that fails at run time: `no such function`
    /// going to SQLite, `function name() does not exist` coming back to
    /// PostgreSQL. A name declared here passes through instead, so declaring it
    /// is a claim about wherever the emitted SQL is going to run, and a caller
    /// who registered a name on the replica alone should not declare it while
    /// reverse translating. Names are matched without regard to case, which is
    /// how SQLite resolves them.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::*;
    ///
    /// let options = Pg2SqliteOptions::default().with_user_defined_functions(["levenshtein"]);
    /// assert!(options.declares_user_defined_function("LEVENSHTEIN"));
    /// assert!(!options.declares_user_defined_function("soundex"));
    /// ```
    #[must_use]
    fn with_user_defined_functions<S: Into<String>>(
        self,
        names: impl IntoIterator<Item = S>,
    ) -> Self;

    /// Returns whether `name` was declared through
    /// [`with_user_defined_functions`](TranslationOptions::with_user_defined_functions).
    fn declares_user_defined_function(&self, name: &str) -> bool;

    /// Enables strict RLS validation: audit rows for writes that are not
    /// readable back through the view are logged with severity `error`
    /// instead of `warning`, and RETURNING-bearing inserts are redirected to
    /// the backing table instead of being refused when a database-filled
    /// column would come back NULL from the view row. Policy enforcement
    /// itself always comes from the emitted guard triggers, in both modes.
    ///
    /// # Example
    /// ```
    /// use pg2sqlite::prelude::*;
    ///
    /// let options = Pg2SqliteOptions::default()
    ///     .with_rls_audit_table_name("rls_violations")
    ///     .with_strict_rls_validation(); // Audit severity becomes error
    /// ```
    #[must_use]
    fn with_strict_rls_validation(self) -> Self;

    /// Returns whether strict RLS validation is enabled.
    fn is_strict_rls_validation(&self) -> bool;

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
    #[must_use]
    fn with_strict_rls_write_deny(self) -> Self;

    /// Returns whether a write denied by a policy's `USING` clause raises
    /// rather than affecting zero rows.
    fn is_strict_rls_write_deny(&self) -> bool;

    /// Declares a SQLite UDF name to use for case folding in ILIKE
    /// translations.
    ///
    /// SQLite's built-in `lower()` folds ASCII only, so `ILIKE` with a pattern
    /// literal containing non-ASCII alphabetic characters is refused unless
    /// this option is set. When set, the named function is called on both
    /// the expression and the pattern instead of `lower()`.
    ///
    /// The caller is responsible for registering the function on the connection
    /// at runtime. A function that folds to lowercase with full Unicode support
    /// (such as one backed by `str::to_lowercase`) satisfies ILIKE semantics.
    #[must_use]
    fn with_ilike_fold_function(self, name: impl Into<String>) -> Self;

    /// Returns the configured ILIKE case-folding function name, if set.
    fn get_ilike_fold_function(&self) -> Option<&str>;
}
