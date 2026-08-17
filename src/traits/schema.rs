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
use sqlparser::{
    ast::{
        BeginEndStatements, CreateFunction, CreateTable, ReturnStatement, ReturnStatementValue,
        Statement, helpers::attached_token::AttachedToken,
    },
    keywords::Keyword,
    tokenizer::{Token, TokenWithSpan, Tokenizer, Word},
};

use crate::{
    errors::Error,
    impls::translator_impls::plpgsql::{PlPgSqlContext, PlPgSqlPreprocessor},
};

/// Trait to define a schema for the translation between `PostgreSQL` and
/// `SQLite`.
pub trait Schema: DatabaseLike<Table = CreateTable, Function = CreateFunction> {
    /// Returns the `BEGIN ... END` body of the named function, if it exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownPostgresFeature`] if the body cannot be
    /// tokenized, parsed, or does not contain a valid `BEGIN ... END`
    /// block.
    fn function_body(&self, name: &str) -> Result<Option<BeginEndStatements>, Error> {
        Ok(self.function_body_with_context(name)?.map(|(body, _context)| body))
    }

    /// Returns a function body plus preprocessed PL/pgSQL declaration context.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownPostgresFeature`] if the function body cannot be
    /// tokenized, parsed, or does not contain a valid `BEGIN ... END` block.
    fn function_body_with_context(
        &self,
        name: &str,
    ) -> Result<Option<(BeginEndStatements, PlPgSqlContext)>, Error> {
        let Some(function) = self.function(name) else {
            return Ok(None);
        };
        let Some(function_body) = function.body() else {
            return Ok(None);
        };

        // We strip spaces and semicolons from the body.
        let maybe_body = function_body.trim().trim_end_matches(';').trim();

        // Reported here rather than left to the SQL parser, which names the
        // statement after the keyword instead of the handler itself.
        if PlPgSqlPreprocessor::has_exception_handler(maybe_body) {
            return Err(Error::UnsupportedSQLiteFeature(format!(
                "trigger function '{name}' uses exception handling, which SQLite has no \
                 equivalent for: a trigger body can abort the statement with RAISE(ABORT) but \
                 cannot catch anything. Move the handling into the application, or drop the \
                 handler and let the error surface."
            )));
        }

        // Preprocess the PL/pgSQL body to handle syntax like `variable := expr`
        let (preprocessed_body, context) = PlPgSqlPreprocessor::preprocess(maybe_body)?;

        let dialect = sqlparser::dialect::PostgreSqlDialect {};
        let tokens = Tokenizer::new(&dialect, &preprocessed_body).tokenize().map_err(|e| {
            Error::UnknownPostgresFeature(format!(
                "Failed to tokenize trigger function '{name}' body: {e}. Body: {preprocessed_body}",
            ))
        })?;

        let begin_idx = tokens
            .iter()
            .position(|t| matches!(t, Token::Word(w) if w.keyword == Keyword::BEGIN))
            .ok_or_else(|| {
                Error::UnknownPostgresFeature(format!(
                    "Trigger function '{name}' body must contain BEGIN...END block. Body: {preprocessed_body}",
                ))
            })?;

        // We look for the last END that is a keyword
        let end_idx = tokens
            .iter()
            .rposition(|t| matches!(t, Token::Word(w) if w.keyword == Keyword::END))
            .ok_or_else(|| {
                Error::UnknownPostgresFeature(format!(
                    "Trigger function '{name}' body must end with END. Body: {preprocessed_body}",
                ))
            })?;

        let body_tokens = tokens[begin_idx + 1..end_idx].to_vec();

        let mut statements = sqlparser::parser::Parser::new(&dialect)
            .with_tokens(body_tokens)
            .parse_statements()
            .map_err(|e| {
                Error::UnknownPostgresFeature(format!(
                    "Failed to parse trigger function '{name}' body statements: {e}. Body: {preprocessed_body}",
                ))
            })?;

        // The function body may end with a `RETURN NEW;` or `RETURN OLD;` statement.
        // If that's the case, we remove it.
        if let Some(Statement::Return(ReturnStatement {
            value: Some(ReturnStatementValue::Expr(expr)),
        })) = statements.last()
        {
            let string_expr = expr.to_string();
            if string_expr == "NEW" || string_expr == "OLD" {
                statements.pop();
            }
        }

        Ok(Some((
            BeginEndStatements {
                begin_token: AttachedToken(TokenWithSpan::wrap(Token::Word(Word {
                    value: "BEGIN".into(),
                    quote_style: None,
                    keyword: Keyword::BEGIN,
                }))),
                statements,
                end_token: AttachedToken(TokenWithSpan::wrap(Token::Word(Word {
                    value: "END".into(),
                    quote_style: None,
                    keyword: Keyword::END,
                }))),
            },
            context,
        )))
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
