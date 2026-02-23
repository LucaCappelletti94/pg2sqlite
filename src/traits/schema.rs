//! Submodule defining a schema for the translation between `PostgreSQL` and
//! `SQLite`.

use sql_traits::traits::{DatabaseLike, FunctionLike};
use sqlparser::{
    ast::{
        BeginEndStatements, CreateFunction, CreateTable, ReturnStatement, ReturnStatementValue,
        Statement, helpers::attached_token::AttachedToken,
    },
    keywords::Keyword,
    tokenizer::{Token, TokenWithSpan, Tokenizer, Word},
};

use crate::{errors::Error, impls::translator_impls::plpgsql::PlPgSqlPreprocessor};

/// Trait to define a schema for the translation between `PostgreSQL` and
/// `SQLite`.
pub trait Schema: DatabaseLike<Table = CreateTable, Function = CreateFunction> {
    /// Returns a reference to the body of a function defined in the schema by
    /// its name, if it exists.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the function to be searched.
    fn function_body(&self, name: &str) -> Result<Option<BeginEndStatements>, Error> {
        let Some(function) = self.function(name) else {
            return Ok(None);
        };
        let Some(function_body) = function.body() else {
            return Ok(None);
        };

        // We strip spaces and semicolons from the body.
        let maybe_body = function_body.trim().trim_end_matches(';').trim();

        // Preprocess the PL/pgSQL body to handle syntax like `variable := expr`
        let (preprocessed_body, _context) = PlPgSqlPreprocessor::preprocess(maybe_body);

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

        Ok(Some(BeginEndStatements {
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
        }))
    }
}

impl<S> Schema for S where S: DatabaseLike<Table = CreateTable, Function = CreateFunction> {}
