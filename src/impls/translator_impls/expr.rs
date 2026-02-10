//! Implementation of the [`Translator`] trait for the
//! `Expr` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::Expr;

use crate::prelude::{Pg2SqliteOptions, Translator};

impl Translator for Expr {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Self;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        Ok(match self {
            Expr::Function(func) => Expr::Function(func.translate(schema, options)?),
            // Pass through simple expressions that work in SQLite
            Expr::Identifier(_) | Expr::Value(_) => self.clone(),
            // Handle unary operators (e.g., -1, NOT x)
            Expr::UnaryOp { op, expr } => {
                Expr::UnaryOp { op: *op, expr: Box::new(expr.translate(schema, options)?) }
            }
            // Handle nested/parenthesized expressions
            Expr::Nested(inner) => Expr::Nested(Box::new(inner.translate(schema, options)?)),
            // Handle binary operations (e.g., 1 + 2, a || b)
            Expr::BinaryOp { left, op, right } => {
                Expr::BinaryOp {
                    left: Box::new(left.translate(schema, options)?),
                    op: op.clone(),
                    right: Box::new(right.translate(schema, options)?),
                }
            }
            // Handle type casts (e.g., value::text)
            Expr::Cast { expr, data_type, format, kind, array } => {
                Expr::Cast {
                    expr: Box::new(expr.translate(schema, options)?),
                    data_type: data_type.translate(schema, options)?,
                    format: format.clone(),
                    kind: kind.clone(),
                    array: *array,
                }
            }
            _ => {
                unimplemented!(
                    "Expr translation for definition `{:?}` is not yet implemented.",
                    self
                )
            }
        })
    }
}
