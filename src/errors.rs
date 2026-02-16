//! Submodule providing the error enumeration that may occur during the
//! translation between `PostgreSQL` and `SQLite`.

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
    /// Error that may occur during the reading of a file.
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    /// Error that may occur during git operations.
    #[error("Git error: {0}")]
    GitError(String),
    /// Error when a function is not available in the schema.
    #[error("Undefined function: {0}")]
    UndefinedFunction(String),
    /// Error when a feature is not supported in `PostgreSQL`.
    #[error("Unknown PostgreSQL feature: {0}")]
    UnknownPostgresFeature(String),
    /// Error when a feature is not supported in `SQLite`.
    #[error("Unsupported SQLite feature: {0}")]
    UnsupportedSQLiteFeature(String),
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
    /// Error when a policy pattern is not supported for translation.
    #[error("Unsupported policy pattern in table '{table}', policy '{policy}': {description}")]
    UnsupportedPolicyPattern {
        /// The table the policy is defined on.
        table: String,
        /// The name of the policy.
        policy: String,
        /// Description of why the pattern is not supported.
        description: String,
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
}
