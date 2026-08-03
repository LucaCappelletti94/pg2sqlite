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
    Parser::parse_sql(dialect, sql).map_err(|error| {
        Error::InternalGeneratedSql {
            context: context.to_string(),
            reason: error.to_string(),
            sql: sql.to_string(),
            source: Some(error),
        }
    })
}

/// Parses generated SQL and expects exactly one statement.
pub(crate) fn parse_single_generated_sql(
    dialect: &dyn Dialect,
    sql: &str,
    context: &str,
) -> Result<Statement, Error> {
    let mut parsed = parse_generated_sql(dialect, sql, context)?;
    if parsed.len() != 1 {
        return Err(Error::InternalGeneratedSql {
            context: context.to_string(),
            reason: format!("expected exactly one statement, got {}", parsed.len()),
            sql: sql.to_string(),
            source: None,
        });
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

    /// The SQL here was written by this crate, not by the caller, so a failure
    /// is a translator bug and has to say so. Reporting it as an unknown
    /// PostgreSQL feature blamed the input for a fault it did not cause.
    #[test]
    fn a_parse_failure_reports_an_internal_fault() {
        let dialect = PostgreSqlDialect {};
        let error = parse_generated_sql(&dialect, "SELEC FROM", "cte translation").unwrap_err();

        assert!(
            matches!(error, crate::errors::Error::InternalGeneratedSql { .. }),
            "expected an internal fault, got {error:?}"
        );
        let message = error.to_string();
        assert!(message.contains("cte translation"), "unexpected error: {message}");
        assert!(message.contains("SELEC FROM"), "unexpected error: {message}");
        assert!(
            !message.contains("Unknown PostgreSQL feature"),
            "the SQL is not PostgreSQL input, so it must not be blamed on one: {message}"
        );
        assert!(
            std::error::Error::source(&error).is_some(),
            "the parser's own error should stay reachable through the source chain"
        );
    }

    /// The other way the generator can be wrong: the SQL parses, but into the
    /// wrong number of statements.
    #[test]
    fn a_statement_count_mismatch_reports_an_internal_fault() {
        let dialect = PostgreSqlDialect {};
        let error = parse_single_generated_sql(&dialect, "SELECT 1; SELECT 2;", "rls view")
            .expect_err("two statements where one was expected");

        assert!(
            matches!(error, crate::errors::Error::InternalGeneratedSql { .. }),
            "expected an internal fault, got {error:?}"
        );
        assert!(error.to_string().contains("rls view"), "unexpected error: {error}");
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
