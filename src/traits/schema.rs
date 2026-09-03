//! Submodule defining a schema for the translation between `PostgreSQL` and
//! `SQLite`.

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

use sql_traits::traits::{DatabaseLike, FunctionLike};
use sqlparser::ast::{BeginEndStatements, CreateFunction, CreateTable};

use crate::{
    errors::Error,
    impls::translator_impls::plpgsql::{PlPgSqlContext, parse_body},
};

/// Trait to define a schema for the translation between `PostgreSQL` and
/// `SQLite`.
pub trait Schema: DatabaseLike<Table = CreateTable, Function = CreateFunction> {
    /// Returns the `BEGIN ... END` body of the named function, if it exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TranslationRefusal`] if the body cannot be
    /// tokenized, parsed, or does not contain a valid `BEGIN ... END`
    /// block.
    fn function_body(&self, name: &str) -> Result<Option<BeginEndStatements>, Error> {
        Ok(self.function_body_with_context(name)?.map(|(body, _context)| body))
    }

    /// Returns a function body plus preprocessed PL/pgSQL declaration context.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TranslationRefusal`] if the function body cannot be
    /// tokenized, parsed, or does not contain a valid `BEGIN ... END` block.
    fn function_body_with_context(
        &self,
        name: &str,
    ) -> Result<Option<(BeginEndStatements, PlPgSqlContext)>, Error> {
        let Some(function) = self.function(None, name) else {
            return Ok(None);
        };
        let Some(function_body) = function.body() else {
            return Ok(None);
        };

        Ok(Some(parse_body(name, function_body)?))
    }
}

impl<S> Schema for S where S: DatabaseLike<Table = CreateTable, Function = CreateFunction> {}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{ast::Statement, dialect::PostgreSqlDialect, parser::Parser};

    use crate::traits::Schema;

    fn parse_statements(sql: &str) -> Vec<Statement> {
        Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse")
    }

    fn schema_with_function(name: &str, body: &str) -> ParserDB {
        let sql =
            format!("CREATE FUNCTION {name}() RETURNS trigger AS $$\n{body}\n$$ LANGUAGE plpgsql;");
        ParserDB::from_statements(parse_statements(&sql), "test".to_string())
            .expect("schema should build")
    }

    #[test]
    fn function_body_strips_trailing_return_new_or_old_statement() {
        let sql = r#"
            CREATE FUNCTION trg_new() RETURNS trigger AS $$
            BEGIN
                INSERT INTO logs(id) VALUES (1);
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;

            CREATE FUNCTION trg_old() RETURNS trigger AS $$
            BEGIN
                DELETE FROM logs WHERE id = 1;
                RETURN OLD;
            END;
            $$ LANGUAGE plpgsql;

            CREATE FUNCTION trg_value() RETURNS trigger AS $$
            BEGIN
                RETURN 1;
            END;
            $$ LANGUAGE plpgsql;
        "#;
        let schema = ParserDB::from_statements(parse_statements(sql), "test".to_string())
            .expect("schema should build");

        let body_new = schema
            .function_body("trg_new")
            .expect("function body extraction should succeed")
            .expect("function should exist");
        assert!(body_new.statements.iter().all(|s| !matches!(s, Statement::Return(_))));

        let body_old = schema
            .function_body("trg_old")
            .expect("function body extraction should succeed")
            .expect("function should exist");
        assert!(body_old.statements.iter().all(|s| !matches!(s, Statement::Return(_))));

        let body_value = schema
            .function_body("trg_value")
            .expect("function body extraction should succeed")
            .expect("function should exist");
        assert!(matches!(body_value.statements.last(), Some(Statement::Return(_))));
    }

    #[test]
    fn function_body_removes_single_trailing_return_new_statement() {
        let sql = r#"
            CREATE FUNCTION trg_only_new() RETURNS trigger AS $$
            BEGIN
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
        "#;
        let schema = ParserDB::from_statements(parse_statements(sql), "test".to_string())
            .expect("schema should build");

        let body = schema
            .function_body("trg_only_new")
            .expect("function body extraction should succeed")
            .expect("function should exist");
        assert!(
            body.statements.is_empty(),
            "the trigger body holds no statement, got {:?}",
            body.statements
        );
    }

    #[test]
    fn function_body_reports_missing_begin_end_and_parse_errors() {
        let missing_begin = schema_with_function("trg_missing_begin", "SELECT 1;");
        let err = missing_begin
            .function_body("trg_missing_begin")
            .expect_err("missing BEGIN should error");
        assert!(err.to_string().contains("must contain BEGIN"));

        let missing_end = schema_with_function("trg_missing_end", "BEGIN\n    SELECT 1;");
        let err =
            missing_end.function_body("trg_missing_end").expect_err("missing END should error");
        assert!(err.to_string().contains("must end with END"));

        let parse_error = schema_with_function("trg_parse_error", "BEGIN\n    SELECT FROM;\nEND;");
        let err = parse_error
            .function_body("trg_parse_error")
            .expect_err("invalid body statements should error");
        assert!(err.to_string().contains("Failed to parse trigger function"));
    }

    #[test]
    fn function_body_reports_tokenizer_errors() {
        let tokenize_error =
            schema_with_function("trg_tokenize_error", "BEGIN\n    SELECT \"unterminated;\nEND;");
        let err = tokenize_error
            .function_body("trg_tokenize_error")
            .expect_err("unterminated quoted identifier should fail tokenization");
        assert!(err.to_string().contains("Failed to tokenize trigger function"));
    }
}
