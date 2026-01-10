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
}
