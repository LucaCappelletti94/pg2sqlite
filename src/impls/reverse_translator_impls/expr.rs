//! Implementation of the [`ReverseTranslator`] trait for the
//! `Expr` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{AccessExpr, Array, Expr};

use super::function::reverse_translate_function;
use crate::{
    errors::Error,
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

impl ReverseTranslator for Expr {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Self;

    #[allow(clippy::too_many_lines)]
    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        Ok(match self {
            Expr::Function(func) => reverse_translate_function(func, schema, options)?,

            // Pass through simple expressions
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Value(_) => self.clone(),

            // Handle unary operators
            Expr::UnaryOp { op, expr } => {
                Expr::UnaryOp { op: *op, expr: Box::new(expr.reverse_translate(schema, options)?) }
            }

            // Handle nested/parenthesized expressions
            Expr::Nested(inner) => {
                Expr::Nested(Box::new(inner.reverse_translate(schema, options)?))
            }

            // Handle binary operations
            Expr::BinaryOp { left, op, right } => {
                Expr::BinaryOp {
                    left: Box::new(left.reverse_translate(schema, options)?),
                    op: op.clone(),
                    right: Box::new(right.reverse_translate(schema, options)?),
                }
            }

            // Handle type casts
            Expr::Cast { expr, data_type, format, kind, array } => {
                Expr::Cast {
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    data_type: data_type.clone(),
                    format: format.clone(),
                    kind: kind.clone(),
                    array: *array,
                }
            }

            // Handle NULL checks
            Expr::IsNull(inner) => {
                Expr::IsNull(Box::new(inner.reverse_translate(schema, options)?))
            }
            Expr::IsNotNull(inner) => {
                Expr::IsNotNull(Box::new(inner.reverse_translate(schema, options)?))
            }

            // Handle boolean checks
            Expr::IsTrue(inner) => {
                Expr::IsTrue(Box::new(inner.reverse_translate(schema, options)?))
            }
            Expr::IsNotTrue(inner) => {
                Expr::IsNotTrue(Box::new(inner.reverse_translate(schema, options)?))
            }
            Expr::IsFalse(inner) => {
                Expr::IsFalse(Box::new(inner.reverse_translate(schema, options)?))
            }
            Expr::IsNotFalse(inner) => {
                Expr::IsNotFalse(Box::new(inner.reverse_translate(schema, options)?))
            }

            // Handle EXISTS subqueries
            Expr::Exists { subquery, negated } => {
                Expr::Exists {
                    subquery: Box::new(subquery.reverse_translate(schema, options)?),
                    negated: *negated,
                }
            }

            // Handle LIKE/ILIKE
            Expr::Like { negated, any, expr, pattern, escape_char } => {
                Expr::Like {
                    negated: *negated,
                    any: *any,
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    pattern: Box::new(pattern.reverse_translate(schema, options)?),
                    escape_char: escape_char.clone(),
                }
            }
            Expr::ILike { negated, any, expr, pattern, escape_char } => {
                Expr::ILike {
                    negated: *negated,
                    any: *any,
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    pattern: Box::new(pattern.reverse_translate(schema, options)?),
                    escape_char: escape_char.clone(),
                }
            }

            // Handle IN list
            Expr::InList { expr, list, negated } => {
                Expr::InList {
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    list: list
                        .iter()
                        .map(|e| e.reverse_translate(schema, options))
                        .collect::<Result<Vec<_>, _>>()?,
                    negated: *negated,
                }
            }

            // Handle IN subquery
            Expr::InSubquery { expr, subquery, negated } => {
                Expr::InSubquery {
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    subquery: Box::new(subquery.reverse_translate(schema, options)?),
                    negated: *negated,
                }
            }

            // Handle BETWEEN
            Expr::Between { expr, negated, low, high } => {
                Expr::Between {
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    negated: *negated,
                    low: Box::new(low.reverse_translate(schema, options)?),
                    high: Box::new(high.reverse_translate(schema, options)?),
                }
            }

            // Handle CASE expressions
            Expr::Case { case_token, end_token, operand, conditions, else_result } => {
                Expr::Case {
                    case_token: case_token.clone(),
                    end_token: end_token.clone(),
                    operand: operand
                        .as_ref()
                        .map(|e| e.reverse_translate(schema, options).map(Box::new))
                        .transpose()?,
                    conditions: conditions
                        .iter()
                        .map(|cw| {
                            Ok(sqlparser::ast::CaseWhen {
                                condition: cw.condition.reverse_translate(schema, options)?,
                                result: cw.result.reverse_translate(schema, options)?,
                            })
                        })
                        .collect::<Result<Vec<_>, Error>>()?,
                    else_result: else_result
                        .as_ref()
                        .map(|e| e.reverse_translate(schema, options).map(Box::new))
                        .transpose()?,
                }
            }

            // Handle scalar subqueries
            Expr::Subquery(query) => {
                Expr::Subquery(Box::new(query.reverse_translate(schema, options)?))
            }

            // Handle EXTRACT (pass through - already PostgreSQL syntax)
            Expr::Extract { field, syntax, expr } => {
                Expr::Extract {
                    field: field.clone(),
                    syntax: syntax.clone(),
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                }
            }

            // Handle tuples
            Expr::Tuple(exprs) => {
                Expr::Tuple(
                    exprs
                        .iter()
                        .map(|e| e.reverse_translate(schema, options))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }

            // Handle arrays
            Expr::Array(Array { elem, named }) => {
                Expr::Array(Array {
                    elem: elem
                        .iter()
                        .map(|e| e.reverse_translate(schema, options))
                        .collect::<Result<Vec<_>, _>>()?,
                    named: *named,
                })
            }

            // Handle TRIM
            Expr::Trim { expr, trim_where, trim_what, trim_characters } => {
                Expr::Trim {
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    trim_where: *trim_where,
                    trim_what: trim_what
                        .as_ref()
                        .map(|e| e.reverse_translate(schema, options).map(Box::new))
                        .transpose()?,
                    trim_characters: trim_characters
                        .as_ref()
                        .map(|chars| {
                            chars
                                .iter()
                                .map(|e| e.reverse_translate(schema, options))
                                .collect::<Result<Vec<_>, _>>()
                        })
                        .transpose()?,
                }
            }

            // Handle CEIL/FLOOR (pass through - SQLite CASE should be recognized)
            Expr::Ceil { expr, field } => {
                Expr::Ceil {
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    field: field.clone(),
                }
            }
            Expr::Floor { expr, field } => {
                Expr::Floor {
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    field: field.clone(),
                }
            }

            // Handle POSITION (pass through - already PostgreSQL syntax)
            Expr::Position { expr, r#in } => {
                Expr::Position {
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    r#in: Box::new(r#in.reverse_translate(schema, options)?),
                }
            }

            // Handle SUBSTRING
            Expr::Substring { expr, substring_from, substring_for, special, shorthand } => {
                Expr::Substring {
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    substring_from: substring_from
                        .as_ref()
                        .map(|e| e.reverse_translate(schema, options).map(Box::new))
                        .transpose()?,
                    substring_for: substring_for
                        .as_ref()
                        .map(|e| e.reverse_translate(schema, options).map(Box::new))
                        .transpose()?,
                    special: *special,
                    shorthand: *shorthand,
                }
            }

            // Handle TypedString
            Expr::TypedString(typed_string) => Expr::TypedString(typed_string.clone()),

            // Handle Prefixed
            Expr::Prefixed { prefix, value } => {
                Expr::Prefixed {
                    prefix: prefix.clone(),
                    value: Box::new(value.reverse_translate(schema, options)?),
                }
            }

            // Handle COLLATE
            Expr::Collate { expr, collation } => {
                Expr::Collate {
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    collation: collation.clone(),
                }
            }

            // Handle INTERVAL
            Expr::Interval(interval) => {
                Expr::Interval(sqlparser::ast::Interval {
                    value: Box::new(interval.value.reverse_translate(schema, options)?),
                    leading_field: interval.leading_field.clone(),
                    leading_precision: interval.leading_precision,
                    last_field: interval.last_field.clone(),
                    fractional_seconds_precision: interval.fractional_seconds_precision,
                })
            }

            // Handle qualified wildcard
            Expr::QualifiedWildcard(name, token) => {
                Expr::QualifiedWildcard(name.clone(), token.clone())
            }

            // Handle RLIKE/REGEXP
            Expr::RLike { negated, expr, pattern, regexp } => {
                Expr::RLike {
                    negated: *negated,
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    pattern: Box::new(pattern.reverse_translate(schema, options)?),
                    regexp: *regexp,
                }
            }

            // Handle compound field access
            Expr::CompoundFieldAccess { root, access_chain } => {
                let translated_chain = access_chain
                    .iter()
                    .map(|access| {
                        match access {
                            AccessExpr::Dot(expr) => {
                                Ok::<_, Error>(AccessExpr::Dot(
                                    expr.reverse_translate(schema, options)?,
                                ))
                            }
                            AccessExpr::Subscript(s) => Ok(AccessExpr::Subscript(s.clone())),
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Expr::CompoundFieldAccess {
                    root: Box::new(root.reverse_translate(schema, options)?),
                    access_chain: translated_chain,
                }
            }

            _ => {
                return Err(Error::UnsupportedSQLiteFeature(format!(
                    "Reverse translation for expression type {:?} is not yet implemented",
                    std::mem::discriminant(self)
                )));
            }
        })
    }
}
