//! Implementation of the [`ReverseTranslator`] trait for the
//! `Expr` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{AccessExpr, Array, Expr, JsonPathElem, Subscript};

use super::function::reverse_translate_function;
use crate::{
    errors::Error,
    impls::shared_helpers::expr_variant_name,
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

fn reverse_translate_access_expr(
    access: &AccessExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<AccessExpr, Error> {
    Ok(match access {
        AccessExpr::Dot(expr) => AccessExpr::Dot(expr.reverse_translate(schema, options)?),
        AccessExpr::Subscript(subscript) => AccessExpr::Subscript(match subscript {
            Subscript::Index { index } => {
                Subscript::Index { index: index.reverse_translate(schema, options)? }
            }
            Subscript::Slice { lower_bound, upper_bound, stride } => Subscript::Slice {
                lower_bound: lower_bound
                    .as_ref()
                    .map(|expr| expr.reverse_translate(schema, options))
                    .transpose()?,
                upper_bound: upper_bound
                    .as_ref()
                    .map(|expr| expr.reverse_translate(schema, options))
                    .transpose()?,
                stride: stride
                    .as_ref()
                    .map(|expr| expr.reverse_translate(schema, options))
                    .transpose()?,
            },
        }),
    })
}

fn reverse_translate_json_path(
    path: &sqlparser::ast::JsonPath,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::JsonPath, Error> {
    Ok(sqlparser::ast::JsonPath {
        path: path
            .path
            .iter()
            .map(|elem| {
                Ok(match elem {
                    JsonPathElem::Dot { key, quoted } => {
                        JsonPathElem::Dot { key: key.clone(), quoted: *quoted }
                    }
                    JsonPathElem::Bracket { key } => {
                        JsonPathElem::Bracket { key: key.reverse_translate(schema, options)? }
                    }
                })
            })
            .collect::<Result<Vec<_>, Error>>()?,
    })
}

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
            Expr::Convert { is_try, expr, data_type, charset, target_before_value, styles } => {
                Expr::Convert {
                    is_try: *is_try,
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    data_type: data_type.clone(),
                    charset: charset.clone(),
                    target_before_value: *target_before_value,
                    styles: styles
                        .iter()
                        .map(|style| style.reverse_translate(schema, options))
                        .collect::<Result<Vec<_>, _>>()?,
                }
            }
            Expr::AtTimeZone { timestamp, time_zone } => {
                Expr::AtTimeZone {
                    timestamp: Box::new(timestamp.reverse_translate(schema, options)?),
                    time_zone: Box::new(time_zone.reverse_translate(schema, options)?),
                }
            }
            Expr::JsonAccess { value, path } => Expr::JsonAccess {
                value: Box::new(value.reverse_translate(schema, options)?),
                path: reverse_translate_json_path(path, schema, options)?,
            },

            // Handle NULL checks
            Expr::IsNull(inner) => {
                Expr::IsNull(Box::new(inner.reverse_translate(schema, options)?))
            }
            Expr::IsNotNull(inner) => {
                Expr::IsNotNull(Box::new(inner.reverse_translate(schema, options)?))
            }
            Expr::IsUnknown(inner) => {
                Expr::IsUnknown(Box::new(inner.reverse_translate(schema, options)?))
            }
            Expr::IsNotUnknown(inner) => {
                Expr::IsNotUnknown(Box::new(inner.reverse_translate(schema, options)?))
            }
            Expr::IsDistinctFrom(left, right) => {
                Expr::IsDistinctFrom(
                    Box::new(left.reverse_translate(schema, options)?),
                    Box::new(right.reverse_translate(schema, options)?),
                )
            }
            Expr::IsNormalized { expr, form, negated } => {
                Expr::IsNormalized {
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    form: *form,
                    negated: *negated,
                }
            }
            Expr::IsNotDistinctFrom(left, right) => {
                Expr::IsNotDistinctFrom(
                    Box::new(left.reverse_translate(schema, options)?),
                    Box::new(right.reverse_translate(schema, options)?),
                )
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
            Expr::SimilarTo { negated, expr, pattern, escape_char } => {
                Expr::SimilarTo {
                    negated: *negated,
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    pattern: Box::new(pattern.reverse_translate(schema, options)?),
                    escape_char: escape_char.clone(),
                }
            }

            // Handle ANY/ALL comparison operations
            Expr::AnyOp { left, compare_op, right, is_some } => {
                Expr::AnyOp {
                    left: Box::new(left.reverse_translate(schema, options)?),
                    compare_op: compare_op.clone(),
                    right: Box::new(right.reverse_translate(schema, options)?),
                    is_some: *is_some,
                }
            }
            Expr::AllOp { left, compare_op, right } => {
                Expr::AllOp {
                    left: Box::new(left.reverse_translate(schema, options)?),
                    compare_op: compare_op.clone(),
                    right: Box::new(right.reverse_translate(schema, options)?),
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
            Expr::InUnnest { expr, array_expr, negated } => Expr::InUnnest {
                expr: Box::new(expr.reverse_translate(schema, options)?),
                array_expr: Box::new(array_expr.reverse_translate(schema, options)?),
                negated: *negated,
            },

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
            Expr::Struct { values, fields } => Expr::Struct {
                values: values
                    .iter()
                    .map(|value| value.reverse_translate(schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                fields: fields.clone(),
            },
            Expr::Named { expr, name } => Expr::Named {
                expr: Box::new(expr.reverse_translate(schema, options)?),
                name: name.clone(),
            },
            Expr::Dictionary(fields) => Expr::Dictionary(
                fields
                    .iter()
                    .map(|field| {
                        Ok(sqlparser::ast::DictionaryField {
                            key: field.key.clone(),
                            value: Box::new(field.value.reverse_translate(schema, options)?),
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()?,
            ),
            Expr::Map(map) => Expr::Map(sqlparser::ast::Map {
                entries: map
                    .entries
                    .iter()
                    .map(|entry| {
                        Ok(sqlparser::ast::MapEntry {
                            key: Box::new(entry.key.reverse_translate(schema, options)?),
                            value: Box::new(entry.value.reverse_translate(schema, options)?),
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()?,
            }),

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
            Expr::Overlay { expr, overlay_what, overlay_from, overlay_for } => {
                Expr::Overlay {
                    expr: Box::new(expr.reverse_translate(schema, options)?),
                    overlay_what: Box::new(overlay_what.reverse_translate(schema, options)?),
                    overlay_from: Box::new(overlay_from.reverse_translate(schema, options)?),
                    overlay_for: overlay_for
                        .as_ref()
                        .map(|expr| expr.reverse_translate(schema, options).map(Box::new))
                        .transpose()?,
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
                    .map(|access| reverse_translate_access_expr(access, schema, options))
                    .collect::<Result<Vec<_>, _>>()?;
                Expr::CompoundFieldAccess {
                    root: Box::new(root.reverse_translate(schema, options)?),
                    access_chain: translated_chain,
                }
            }
            Expr::OuterJoin(inner) => {
                Expr::OuterJoin(Box::new(inner.reverse_translate(schema, options)?))
            }
            Expr::Prior(inner) => Expr::Prior(Box::new(inner.reverse_translate(schema, options)?)),
            Expr::Lambda(lambda) => Expr::Lambda(sqlparser::ast::LambdaFunction {
                params: lambda.params.clone(),
                body: Box::new(lambda.body.reverse_translate(schema, options)?),
                syntax: lambda.syntax,
            }),
            Expr::MemberOf(member) => Expr::MemberOf(sqlparser::ast::MemberOf {
                value: Box::new(member.value.reverse_translate(schema, options)?),
                array: Box::new(member.array.reverse_translate(schema, options)?),
            }),

            _ => {
                let variant_name = expr_variant_name(self);
                return Err(Error::UnsupportedSQLiteFeature(format!(
                    "Reverse translation for expression variant `{variant_name}` is not yet implemented",
                )));
            }
        })
    }
}
