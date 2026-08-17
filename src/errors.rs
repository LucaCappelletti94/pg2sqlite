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

use sqlparser::parser::ParserError;

#[derive(Debug, thiserror::Error)]
/// Error enumeration that may occur during the translation between `PostgreSQL`
/// and `SQLite`.
pub enum Error {
    /// Error that may occur during the parsing of a SQL statement.
    #[error("Parser error in '{0}': {1}")]
    ParserError(String, ParserError),
    /// Error that may occur during the construction of the schema.
    #[error("Schema error: {0}")]
    SchemaError(#[from] sql_traits::errors::Error),
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
    #[error("Git error: {0}")]
    GitError(String),
    /// SQL this crate generated could not be read back.
    ///
    /// Not a defect in the input. The statement was synthesised by the
    /// translator, so reaching this means the generator emitted something it
    /// cannot itself parse, and the translation that depends on it cannot be
    /// trusted. Reported rather than panicked because a library must not abort
    /// its caller's process over its own bug.
    #[error(
        "Internal pg2sqlite fault while {context}: {reason}. \
         This is a bug in pg2sqlite rather than in the input. Generated SQL: {sql}"
    )]
    InternalGeneratedSql {
        /// What the translator was generating, such as `RLS view`.
        context: String,
        /// Why the generated SQL could not be read back.
        reason: String,
        /// The SQL the translator produced.
        sql: String,
        /// The parser's own error, when the failure was a parse failure. A
        /// statement-count mismatch has no underlying error, so it is `None`.
        #[source]
        source: Option<ParserError>,
    },
    /// Error when a feature is not supported in `PostgreSQL`.
    #[error("Unknown PostgreSQL feature: {0}")]
    UnknownPostgresFeature(String),
    /// Error when a feature is not supported in `SQLite`.
    #[error("Unsupported SQLite feature: {0}")]
    UnsupportedSQLiteFeature(String),
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
        source: Option<ParserError>,
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
    /// Error when an unqualified table reference matches multiple schema
    /// tables.
    #[error(
        "Table '{table_name}' is ambiguous across schemas: {schemas:?}. \
         Please qualify the table name with a schema."
    )]
    AmbiguousTableInSchema {
        /// The unqualified table name that matched multiple schema entries.
        table_name: String,
        /// Candidate schema names where the table exists.
        schemas: Vec<String>,
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
         Example: .with_rls_audit_table_name(\"rls_violations\".to_string())"
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
