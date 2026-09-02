//! Submodule providing the error enumeration that may occur during the
//! translation between `PostgreSQL` and `SQLite`.

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
use core::fmt;

use sqlparser::parser::ParserError;

/// Direction of a refused translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationDirection {
    /// PostgreSQL input targeting SQLite.
    PostgreSqlToSqlite,
    /// SQLite input targeting PostgreSQL.
    SqliteToPostgreSql,
}

/// Stable reason category for a refused translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalCategory {
    /// The source construct is outside the accepted source language.
    UnsupportedSourceSyntax,
    /// The target cannot preserve the source construct's meaning.
    UnrepresentableSemantics,
}

/// A translation refusal with matchable direction and reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationRefusal {
    direction: TranslationDirection,
    category: RefusalCategory,
    detail: String,
}

impl TranslationRefusal {
    pub(crate) fn new(
        direction: TranslationDirection,
        category: RefusalCategory,
        detail: impl Into<String>,
    ) -> Self {
        Self { direction, category, detail: detail.into() }
    }

    /// Returns the refused translation direction.
    #[must_use]
    pub const fn direction(&self) -> TranslationDirection {
        self.direction
    }

    /// Returns the stable refusal category.
    #[must_use]
    pub const fn category(&self) -> RefusalCategory {
        self.category
    }

    /// Returns the detailed refusal message.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for TranslationRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let target = match self.direction {
            TranslationDirection::PostgreSqlToSqlite => "SQLite",
            TranslationDirection::SqliteToPostgreSql => "PostgreSQL",
        };
        write!(formatter, "Unsupported {target} feature: {}", self.detail)
    }
}

impl core::error::Error for TranslationRefusal {}

/// A SQL parse error whose parser implementation remains private.
#[derive(Debug, thiserror::Error)]
#[error("Parser error in '{input}': {source}")]
pub struct SqlParseError {
    input: String,
    #[source]
    source: ParserError,
}

impl SqlParseError {
    pub(crate) fn new(input: impl Into<String>, source: ParserError) -> Self {
        Self { input: input.into(), source }
    }

    /// Returns the SQL text that failed to parse.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }
}

#[derive(Debug, thiserror::Error)]
/// Error enumeration that may occur during the translation between `PostgreSQL`
/// and `SQLite`.
pub enum Error {
    /// SQL input could not be parsed.
    #[error(transparent)]
    SqlParse(#[from] SqlParseError),
    /// Error that may occur during the construction of the schema.
    ///
    /// Boxed because the nested enum is 120 bytes, which would put this enum
    /// at clippy's `result_large_err` threshold and copy that weight through
    /// every `Result` in the crate.
    #[error("Schema error: {0}")]
    SchemaError(#[source] Box<sql_traits::errors::Error>),
    /// Error raised when a schema object cannot be resolved in the database it
    /// is queried against.
    ///
    /// Carried separately from [`Error::SchemaError`] rather than nested inside
    /// it so that `?` converts a `LookupError` in one hop. The accessors on
    /// `sql-traits`' `TableLike` and `ColumnLike` return this whenever the
    /// object is absent, which happens for instance when a statement list
    /// renames a table away and a caller still holds the original `CREATE
    /// TABLE` node.
    #[error("Schema lookup error: {0}")]
    LookupError(#[from] sql_traits::errors::LookupError),
    /// Error that may occur during the reading of a file.
    ///
    /// Only available with the `std` feature; produced by the
    /// filesystem-backed loaders on `Pg2Sqlite` (`file`, `ups`, `ups_until`,
    /// `from_git`), which are themselves `std`-gated.
    #[cfg(feature = "std")]
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    /// Error that may occur during git operations.
    #[cfg(feature = "git")]
    #[error("Git error: {0}")]
    GitError(String),
    /// Translation cannot preserve the source construct.
    #[error(transparent)]
    TranslationRefusal(#[from] TranslationRefusal),
    /// Error when reverse RLS scanning encounters an expression variant that is
    /// not explicitly handled.
    #[error(
        "Reverse RLS scanner encountered unsupported expression variant '{expr_variant}'. \
         Refusing to continue to avoid fail-open behavior. Expression: {expression}"
    )]
    UnsupportedRlsExpressionVariant {
        /// Expression variant name.
        expr_variant: String,
        /// SQL rendering of the expression.
        expression: String,
    },
    /// Error when a session variable pattern is encountered but no mapping is
    /// configured.
    #[error(
        "No session variable mapping configured for pattern '{pattern}'. \
         Configure a mapping using `with_session_variable()` or `with_session_user()` \
         in the translation options."
    )]
    SessionVariableMappingNotFound {
        /// The PostgreSQL pattern that was encountered.
        pattern: String,
    },
    /// Error when the type recorded on a session variable mapping cannot be
    /// read as a type.
    #[error(
        "The session variable mapping for '{pattern}' records the PostgreSQL type \
         '{pg_type}', which does not parse as one. Record it as PostgreSQL spells it, such as \
         `uuid` or `numeric(10,2)`."
    )]
    SessionVariableTypeUnreadable {
        /// The PostgreSQL pattern the mapping matches.
        pattern: String,
        /// The type spelling the mapping carries.
        pg_type: String,
        /// The parser's own error, when the spelling failed to parse at all. A
        /// spelling that parses and then leaves input behind, such as `uuid
        /// oops`, has no underlying error, so it is `None`.
        #[source]
        source: Option<SqlParseError>,
    },
    /// Error when a statement casts a session variable to a type other than the
    /// one its mapping records.
    #[error(
        "The session variable mapping for '{pattern}' records the PostgreSQL type \
         '{recorded}', and this statement casts the setting to '{written}'. The cast is dropped \
         going to SQLite and written again from the recorded type coming back, so the two have \
         to agree: correct whichever is stale."
    )]
    SessionVariableTypeDisagrees {
        /// The PostgreSQL pattern the mapping matches.
        pattern: String,
        /// The type the mapping records.
        recorded: String,
        /// The type the statement casts to.
        written: String,
    },
    /// Error when attempting to access an RLS backing table directly.
    #[error(
        "Direct access to RLS backing table '{table_name}' is not allowed. \
         Tables with suffix '{suffix}' are internal RLS tables. \
         Use the corresponding view instead."
    )]
    RlsTableDetected {
        /// The name of the RLS backing table that was accessed.
        table_name: String,
        /// The RLS table suffix that was detected.
        suffix: String,
    },
    /// Error when the configured RLS backing table suffix cannot separate a
    /// backing table from the view that replaces it.
    #[error(
        "The RLS backing table suffix is empty, so a secured table and its view would take \
         the same name and every name would read as a backing table. Set a non-empty suffix \
         with with_rls_table_suffix, or leave the default '_rls'."
    )]
    EmptyRlsTableSuffix,
    /// Error when a column reference cannot be resolved to a declared column
    /// through the relations in scope where it appears.
    #[error(
        "{reference} cannot be resolved to a declared column: {reason}. This translation depends \
         on the column's declared type, and reading the type off any table in the schema that \
         happens to carry the name would answer with another table's column. Include the \
         column's table in the translation batch, or qualify the reference so it names one \
         relation."
    )]
    UnresolvedColumnReference {
        /// The reference as the statement wrote it.
        reference: String,
        /// Why the relations in scope cannot answer it.
        reason: String,
    },
    /// Error when attempting to reverse translate a non-DML statement.
    #[error(
        "Reverse translation only supports DML statements (INSERT, UPDATE, DELETE, SELECT). \
         Received: {statement_type}"
    )]
    UnsupportedReverseStatement {
        /// The type of statement that was attempted.
        statement_type: String,
    },
    /// Error when reverse translation encounters a SQLite named bind
    /// placeholder.
    ///
    /// SQLite accepts the named forms `:name`, `@name`, and `$name`, but the
    /// PostgreSQL wire protocol has only numbered `$N` parameters, so a named
    /// placeholder cannot be translated. diesel never emits these forms.
    #[error(
        "Named bind placeholder '{placeholder}' is not supported in reverse translation. \
         PostgreSQL accepts only numbered parameters ($N); use positional (?) or numbered (?N)."
    )]
    UnsupportedNamedPlaceholder {
        /// The named placeholder token that was encountered.
        placeholder: String,
    },
    /// Error when a table referenced in reverse translation is not found in the
    /// schema.
    #[error(
        "Table '{table_name}' not found in schema. \
         Reverse translation requires schema information for accurate type recovery."
    )]
    TableNotFoundInSchema {
        /// The name of the table that was not found.
        table_name: String,
    },
    /// Error when two objects the translation emits would carry the same
    /// SQLite name.
    #[error(
        "Two {kind} in the emitted schema are both named '{name}', one from {first} and one \
         from {second}. {reason}, so the second definition cannot apply. Rename one of them at \
         the source."
    )]
    EmittedNameCollision {
        /// What the two definitions are, in the plural.
        kind: String,
        /// The SQLite name both would take.
        name: String,
        /// Where the definition that holds the name came from.
        first: String,
        /// Where the definition that could not have it came from.
        second: String,
        /// Why the two namespaces disagree.
        reason: String,
    },
    /// Error when an upsert (INSERT OR REPLACE) does not include primary key
    /// columns.
    #[error(
        "INSERT OR REPLACE on table '{table_name}' must include primary key columns {pk_columns:?}. \
         Found columns: {insert_columns:?}"
    )]
    MissingPrimaryKeyInUpsert {
        /// The name of the table being inserted into.
        table_name: String,
        /// The primary key columns that are required.
        pk_columns: Vec<String>,
        /// The columns that were provided in the INSERT statement.
        insert_columns: Vec<String>,
    },
    /// Error when RLS tables are present but no audit table name is configured.
    #[error(
        "RLS audit table name must be configured via `with_rls_audit_table_name()` \
         when translating schemas with RLS policies. \
         Example: .with_rls_audit_table_name(\"rls_violations\")"
    )]
    RlsAuditTableNameRequired,
    /// Error when a migration file is not found among discovered migrations.
    #[error("stop_at path '{path}' was not found among discovered up.sql migrations")]
    MigrationNotFound {
        /// The path that was not found.
        path: String,
    },
    /// Error when an object name uses unsupported schema qualification.
    #[error(
        "Unsupported schema-qualified object name '{object_name}': {reason}. \
         Forward translation accepts explicit schemas only when they resolve in the input schema."
    )]
    UnsupportedSchemaQualification {
        /// The original object name.
        object_name: String,
        /// Reason why this qualification is unsupported.
        reason: String,
    },
    /// Error when a generated read-only deny trigger would collide with an
    /// object that the input schema already defines.
    #[error(
        "Cannot generate read-only deny trigger '{trigger_name}' for table '{table_name}': \
         the input schema already defines an object with that name. \
         Rename the conflicting object."
    )]
    ReadonlyDenyTriggerNameCollision {
        /// The read-only table the deny triggers protect.
        table_name: String,
        /// The reserved trigger name that collides with an existing object.
        trigger_name: String,
    },
}
impl Error {
    pub(crate) fn unsupported_source_syntax(detail: impl Into<String>) -> Self {
        TranslationRefusal::new(
            TranslationDirection::PostgreSqlToSqlite,
            RefusalCategory::UnsupportedSourceSyntax,
            detail,
        )
        .into()
    }
    pub(crate) fn reverse_unsupported_source_syntax(detail: impl Into<String>) -> Self {
        TranslationRefusal::new(
            TranslationDirection::SqliteToPostgreSql,
            RefusalCategory::UnsupportedSourceSyntax,
            detail,
        )
        .into()
    }

    pub(crate) fn forward_refusal(detail: impl Into<String>) -> Self {
        TranslationRefusal::new(
            TranslationDirection::PostgreSqlToSqlite,
            RefusalCategory::UnrepresentableSemantics,
            detail,
        )
        .into()
    }

    pub(crate) fn reverse_refusal(detail: impl Into<String>) -> Self {
        TranslationRefusal::new(
            TranslationDirection::SqliteToPostgreSql,
            RefusalCategory::UnrepresentableSemantics,
            detail,
        )
        .into()
    }
}

/// Boxes on the way in, so `?` still converts a schema error in one hop.
impl From<sql_traits::errors::Error> for Error {
    fn from(error: sql_traits::errors::Error) -> Self {
        Self::SchemaError(Box::new(error))
    }
}

/// Routes the extracted PL/pgSQL errors onto the two refusal categories this
/// crate already uses.
///
/// Semantics SQLite cannot represent become `UnrepresentableSemantics`, and
/// input it cannot read becomes `UnsupportedSourceSyntax`. The wording is kept
/// as it was before the extraction, because tests and users match on it.
impl From<sqlparser_plpgsql::Error> for Error {
    fn from(error: sqlparser_plpgsql::Error) -> Self {
        use sqlparser_plpgsql::Error as Extracted;
        match error {
            Extracted::ExceptionHandler { name } => {
                Self::forward_refusal(format!(
                    "trigger function '{name}' uses exception handling, which SQLite has no \
                 equivalent for: a trigger body can abort the statement with RAISE(ABORT) but \
                 cannot catch anything. Move the handling into the application, or drop the \
                 handler and let the error surface."
                ))
            }
            // NEW names the row the trigger writes, so it gets the message
            // about emulation by UPDATE. Every other qualifier is a plpgsql
            // record local, which SQLite has nothing to hold.
            Extracted::QualifiedAssignment { qualifier, name }
                if qualifier.eq_ignore_ascii_case("new") =>
            {
                Self::forward_refusal(format!(
                    "assigning to `{qualifier}.{name}` has no SQLite equivalent here, since a \
                     SQLite trigger cannot change the row it fired for. It is emulated with an \
                     UPDATE over the written row, which needs the whole trigger function body to \
                     be a run of assignments to columns the table declares, closed by RETURN NEW."
                ))
            }
            Extracted::QualifiedAssignment { qualifier, name } => {
                Self::forward_refusal(format!(
                    "assigning to `{qualifier}.{name}` has no SQLite equivalent, since a SQLite \
                 trigger body has no plpgsql record variable to hold the change. Compute the \
                 value in the statement that reads it."
                ))
            }
            Extracted::UnsupportedRaiseUsing { clause } => {
                Self::forward_refusal(format!(
                    "RAISE EXCEPTION USING {clause} is not supported; only USING MESSAGE = \
                 '<string literal>' translates to SQLite"
                ))
            }
            Extracted::Tokenization { name, body, source } => {
                Self::unsupported_source_syntax(format!(
                    "Failed to tokenize trigger function '{name}' body: {source}. Body: {body}"
                ))
            }
            Extracted::MissingBeginBlock { name, body } => {
                Self::unsupported_source_syntax(format!(
                    "Trigger function '{name}' body must contain BEGIN...END block. Body: {body}"
                ))
            }
            Extracted::MissingEndBlock { name, body } => {
                Self::unsupported_source_syntax(format!(
                    "Trigger function '{name}' body must end with END. Body: {body}"
                ))
            }
            Extracted::UnterminatedDollarQuote { name } => {
                Self::unsupported_source_syntax(format!(
                    "Trigger function '{name}' body has an unterminated dollar-quoted string"
                ))
            }
            Extracted::ParseStatements { name, body, source } => {
                Self::unsupported_source_syntax(format!(
                    "Failed to parse trigger function '{name}' body statements: {source}. Body: \
                     {body}"
                ))
            }
        }
    }
}
