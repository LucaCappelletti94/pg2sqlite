//! Implementation of the [`ReverseTranslator`] trait for the
//! `Expr` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::Expr;

use super::{function::reverse_translate_function, helpers::Reverse};
use crate::{
    errors::Error,
    impls::shared_helpers::translate_expr_recursive,
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

impl ReverseTranslator for Expr {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Self;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        match self {
            Expr::Function(func) => reverse_translate_function(func, schema, options),
            _ => translate_expr_recursive::<Reverse>(self, schema, options),
        }
    }
}
