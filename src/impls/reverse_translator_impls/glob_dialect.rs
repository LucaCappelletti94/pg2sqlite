//! SQLite dialect extended so `GLOB` parses as an operator.
//!
//! `GLOB` is a recognised `Keyword` with `Like`-level precedence, but
//! `SQLiteDialect` has no `parse_infix` arm for it, so the parser reaches
//! `GLOB` and fails. The reverse translator needs the node in order to convert
//! it to `LIKE ... ESCAPE`, so this wrapper adds the missing arm.
//!
//! Interim. The optimal fix is upstream in
//! apache/datafusion-sqlparser-rs, where `SQLiteDialect::parse_infix` should
//! handle `GLOB` alongside `MATCH` and `REGEXP`, at which point this file goes
//! away.
//!
//! Every other method delegates to an inner [`SQLiteDialect`] rather than
//! copying its body. A copy silently drifts: the first version of this file
//! reproduced fourteen of the dialect's overrides and missed
//! `supports_numeric_literal_underscores`, so `1_000` stopped parsing here
//! while still parsing in real SQLite.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{boxed::Box, string::ToString};

use sqlparser::{
    ast::{BinaryOperator, Expr, Statement},
    dialect::{Dialect, SQLiteDialect},
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::Token,
};

/// [`SQLiteDialect`] plus `GLOB` infix parsing.
#[derive(Debug, Default)]
pub(crate) struct SqliteGlobDialect {
    inner: SQLiteDialect,
}

impl Dialect for SqliteGlobDialect {
    fn parse_infix(
        &self,
        parser: &mut Parser,
        expr: &Expr,
        precedence: u8,
    ) -> Option<Result<Expr, ParserError>> {
        // Take GLOB before delegating, since the inner dialect would decline it
        // and leave the main parser to fail on an unhandled infix keyword.
        if let Token::Word(word) = &parser.peek_token_ref().token
            && word.keyword == Keyword::GLOB
        {
            parser.advance_token();
            return Some(parser.parse_subexpr(precedence).map(|right| {
                Expr::BinaryOp {
                    left: Box::new(expr.clone()),
                    op: BinaryOperator::Custom("GLOB".to_string()),
                    right: Box::new(right),
                }
            }));
        }
        self.inner.parse_infix(parser, expr, precedence)
    }

    fn is_delimited_identifier_start(&self, ch: char) -> bool {
        self.inner.is_delimited_identifier_start(ch)
    }

    fn identifier_quote_style(&self, identifier: &str) -> Option<char> {
        self.inner.identifier_quote_style(identifier)
    }

    fn is_identifier_start(&self, ch: char) -> bool {
        self.inner.is_identifier_start(ch)
    }

    fn is_identifier_part(&self, ch: char) -> bool {
        self.inner.is_identifier_part(ch)
    }

    fn supports_filter_during_aggregation(&self) -> bool {
        self.inner.supports_filter_during_aggregation()
    }

    fn supports_start_transaction_modifier(&self) -> bool {
        self.inner.supports_start_transaction_modifier()
    }

    fn parse_statement(&self, parser: &mut Parser) -> Option<Result<Statement, ParserError>> {
        self.inner.parse_statement(parser)
    }

    fn supports_in_empty_list(&self) -> bool {
        self.inner.supports_in_empty_list()
    }

    fn supports_limit_comma(&self) -> bool {
        self.inner.supports_limit_comma()
    }

    fn supports_asc_desc_in_column_definition(&self) -> bool {
        self.inner.supports_asc_desc_in_column_definition()
    }

    fn supports_dollar_placeholder(&self) -> bool {
        self.inner.supports_dollar_placeholder()
    }

    fn supports_notnull_operator(&self) -> bool {
        self.inner.supports_notnull_operator()
    }

    fn supports_comma_separated_trim(&self) -> bool {
        self.inner.supports_comma_separated_trim()
    }

    fn supports_numeric_literal_underscores(&self) -> bool {
        self.inner.supports_numeric_literal_underscores()
    }
}
