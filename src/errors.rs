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
}
