//! Utilities for parsing generated SQL snippets into AST statements.

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
    Ok(parsed.remove(0))
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sqlparser::{ast::Statement, dialect::PostgreSqlDialect};

    use super::{parse_generated_sql, parse_single_generated_sql};

    #[test]
    fn parse_generated_sql_parses_multiple_statements() {
        let dialect = PostgreSqlDialect {};
        let parsed = parse_generated_sql(&dialect, "SELECT 1; SELECT 2;", "unit-test").unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn parse_generated_sql_returns_context_in_error_message() {
        let dialect = PostgreSqlDialect {};
        let err = parse_generated_sql(&dialect, "SELEC FROM", "cte translation").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cte translation"), "unexpected error: {msg}");
        assert!(msg.contains("SELEC FROM"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_single_generated_sql_parses_exactly_one_statement() {
        let dialect = PostgreSqlDialect {};
        let parsed = parse_single_generated_sql(&dialect, "SELECT 1;", "single").unwrap();
        assert!(matches!(parsed, Statement::Query(_)));
    }

    #[test]
    fn parse_single_generated_sql_rejects_zero_or_multiple_statements() {
        let dialect = PostgreSqlDialect {};

        let zero = parse_single_generated_sql(&dialect, "   ", "zero");
        assert!(zero.is_err(), "expected empty SQL to fail");

        let many = parse_single_generated_sql(&dialect, "SELECT 1; SELECT 2;", "many");
        assert!(many.is_err(), "expected multiple statements to fail");
    }
}
