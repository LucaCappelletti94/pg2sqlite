//! Utilities for parsing generated SQL snippets into AST statements.

use sqlparser::{ast::Statement, dialect::Dialect, parser::Parser};

use crate::errors::Error;

/// Parses generated SQL and returns all parsed statements.
///
/// This helper is used by translators that synthesize SQL fragments and need
/// strict parse validation instead of silently dropping invalid SQL.
pub(crate) fn parse_generated_sql(
    dialect: &dyn Dialect,
    sql: &str,
    context: &str,
) -> Result<Vec<Statement>, Error> {
    Parser::parse_sql(dialect, sql)
        .map_err(|e| Error::UnknownPostgresFeature(format!("{context}: {e}. SQL: {sql}")))
}

/// Parses generated SQL and expects exactly one statement.
pub(crate) fn parse_single_generated_sql(
    dialect: &dyn Dialect,
    sql: &str,
    context: &str,
) -> Result<Statement, Error> {
    let mut parsed = parse_generated_sql(dialect, sql, context)?;
    if parsed.len() != 1 {
        return Err(Error::UnknownPostgresFeature(format!(
            "{context}: expected exactly one statement, got {}. SQL: {sql}",
            parsed.len()
        )));
    }
    parsed.pop().ok_or_else(|| {
        Error::UnknownPostgresFeature(format!(
            "{context}: parser returned no statements. SQL: {sql}"
        ))
    })
}
