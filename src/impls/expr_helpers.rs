//! Shared expression walker/visitor helpers.
//!
//! These helpers capture the structural recursion over [`Expr`] variants so
//! that each specific walker only needs to handle its "interesting" arms and
//! can delegate the mechanical child-traversal to one of these functions.

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

use sqlparser::ast::{CaseWhen, Expr, JsonPathElem, helpers::attached_token::AttachedToken};

/// Apply `f` to every direct child [`Expr`], rebuilding the node. Callers
/// should match their "interesting" variants first and fall through for the
/// rest.
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
pub(crate) fn map_expr_children(expr: &Expr, f: &impl Fn(&Expr) -> Expr) -> Expr {
    match expr {
        Expr::Identifier(_)
        | Expr::CompoundIdentifier(_)
        | Expr::Value(_)
        | Expr::TypedString(_)
        | Expr::Wildcard(_)
        | Expr::QualifiedWildcard(..)
        | Expr::MatchAgainst { .. }
        | Expr::Lambda(_)
        | Expr::MemberOf(_) => expr.clone(),

        Expr::IsFalse(e) => Expr::IsFalse(Box::new(f(e))),
        Expr::IsNotFalse(e) => Expr::IsNotFalse(Box::new(f(e))),
        Expr::IsTrue(e) => Expr::IsTrue(Box::new(f(e))),
        Expr::IsNotTrue(e) => Expr::IsNotTrue(Box::new(f(e))),
        Expr::IsNull(e) => Expr::IsNull(Box::new(f(e))),
        Expr::IsNotNull(e) => Expr::IsNotNull(Box::new(f(e))),
        Expr::IsUnknown(e) => Expr::IsUnknown(Box::new(f(e))),
        Expr::IsNotUnknown(e) => Expr::IsNotUnknown(Box::new(f(e))),
        Expr::Nested(e) => Expr::Nested(Box::new(f(e))),
        Expr::OuterJoin(e) => Expr::OuterJoin(Box::new(f(e))),
        Expr::Prior(e) => Expr::Prior(Box::new(f(e))),
        Expr::Prefixed { prefix, value } => {
            Expr::Prefixed { prefix: prefix.clone(), value: Box::new(f(value)) }
        }
        Expr::Named { expr: inner, name } => {
            Expr::Named { expr: Box::new(f(inner)), name: name.clone() }
        }
        Expr::IsNormalized { expr: inner, form, negated } => {
            Expr::IsNormalized { expr: Box::new(f(inner)), form: *form, negated: *negated }
        }
        Expr::IsJson { expr: inner, kind, unique_keys, negated } => {
            Expr::IsJson {
                expr: Box::new(f(inner)),
                kind: *kind,
                unique_keys: *unique_keys,
                negated: *negated,
            }
        }
        Expr::UnaryOp { op, expr: inner } => Expr::UnaryOp { op: *op, expr: Box::new(f(inner)) },
        Expr::Cast { kind, expr: inner, data_type, format } => {
            Expr::Cast {
                kind: kind.clone(),
                expr: Box::new(f(inner)),
                data_type: data_type.clone(),
                format: format.clone(),
            }
        }
        Expr::Extract { field, syntax, expr: inner } => {
            Expr::Extract { field: field.clone(), syntax: syntax.clone(), expr: Box::new(f(inner)) }
        }
        Expr::Ceil { expr: inner, field } => {
            Expr::Ceil { expr: Box::new(f(inner)), field: field.clone() }
        }
        Expr::Floor { expr: inner, field } => {
            Expr::Floor { expr: Box::new(f(inner)), field: field.clone() }
        }
        Expr::Collate { expr: inner, collation } => {
            Expr::Collate { expr: Box::new(f(inner)), collation: collation.clone() }
        }
        Expr::Convert { is_try, expr: inner, data_type, charset, target_before_value, styles } => {
            Expr::Convert {
                is_try: *is_try,
                expr: Box::new(f(inner)),
                data_type: data_type.clone(),
                charset: charset.clone(),
                target_before_value: *target_before_value,
                styles: styles.iter().map(f).collect(),
            }
        }

        Expr::IsDistinctFrom(a, b) => Expr::IsDistinctFrom(Box::new(f(a)), Box::new(f(b))),
        Expr::IsNotDistinctFrom(a, b) => Expr::IsNotDistinctFrom(Box::new(f(a)), Box::new(f(b))),
        Expr::BinaryOp { left, op, right } => {
            Expr::BinaryOp { left: Box::new(f(left)), op: op.clone(), right: Box::new(f(right)) }
        }
        Expr::AnyOp { left, compare_op, right, is_some } => {
            Expr::AnyOp {
                left: Box::new(f(left)),
                compare_op: compare_op.clone(),
                right: Box::new(f(right)),
                is_some: *is_some,
            }
        }
        Expr::AllOp { left, compare_op, right } => {
            Expr::AllOp {
                left: Box::new(f(left)),
                compare_op: compare_op.clone(),
                right: Box::new(f(right)),
            }
        }
        Expr::Like { negated, any, expr: inner, pattern, escape_char } => {
            Expr::Like {
                negated: *negated,
                any: *any,
                expr: Box::new(f(inner)),
                pattern: Box::new(f(pattern)),
                escape_char: escape_char.clone(),
            }
        }
        Expr::ILike { negated, any, expr: inner, pattern, escape_char } => {
            Expr::ILike {
                negated: *negated,
                any: *any,
                expr: Box::new(f(inner)),
                pattern: Box::new(f(pattern)),
                escape_char: escape_char.clone(),
            }
        }
        Expr::SimilarTo { negated, expr: inner, pattern, escape_char } => {
            Expr::SimilarTo {
                negated: *negated,
                expr: Box::new(f(inner)),
                pattern: Box::new(f(pattern)),
                escape_char: escape_char.clone(),
            }
        }
        Expr::RLike { negated, expr: inner, pattern, regexp } => {
            Expr::RLike {
                negated: *negated,
                expr: Box::new(f(inner)),
                pattern: Box::new(f(pattern)),
                regexp: *regexp,
            }
        }
        Expr::AtTimeZone { timestamp, time_zone } => {
            Expr::AtTimeZone {
                timestamp: Box::new(f(timestamp)),
                time_zone: Box::new(f(time_zone)),
            }
        }
        Expr::Position { expr: inner, r#in } => {
            Expr::Position { expr: Box::new(f(inner)), r#in: Box::new(f(r#in)) }
        }

        Expr::Between { expr: inner, negated, low, high } => {
            Expr::Between {
                expr: Box::new(f(inner)),
                negated: *negated,
                low: Box::new(f(low)),
                high: Box::new(f(high)),
            }
        }
        Expr::Overlay { expr: inner, overlay_what, overlay_from, overlay_for } => {
            Expr::Overlay {
                expr: Box::new(f(inner)),
                overlay_what: Box::new(f(overlay_what)),
                overlay_from: Box::new(f(overlay_from)),
                overlay_for: overlay_for.as_ref().map(|e| Box::new(f(e))),
            }
        }

        Expr::InList { expr: inner, list, negated } => {
            Expr::InList {
                expr: Box::new(f(inner)),
                list: list.iter().map(f).collect(),
                negated: *negated,
            }
        }
        Expr::Tuple(items) => Expr::Tuple(items.iter().map(f).collect()),
        Expr::Array(arr) => {
            Expr::Array(sqlparser::ast::Array {
                elem: arr.elem.iter().map(f).collect(),
                named: arr.named,
            })
        }
        Expr::GroupingSets(sets) => {
            Expr::GroupingSets(sets.iter().map(|s| s.iter().map(f).collect()).collect())
        }
        Expr::Cube(sets) => Expr::Cube(sets.iter().map(|s| s.iter().map(f).collect()).collect()),
        Expr::Rollup(sets) => {
            Expr::Rollup(sets.iter().map(|s| s.iter().map(f).collect()).collect())
        }
        Expr::Struct { values, fields } => {
            Expr::Struct { values: values.iter().map(f).collect(), fields: fields.clone() }
        }

        Expr::Substring { expr: inner, substring_from, substring_for, special, shorthand } => {
            Expr::Substring {
                expr: Box::new(f(inner)),
                substring_from: substring_from.as_ref().map(|e| Box::new(f(e))),
                substring_for: substring_for.as_ref().map(|e| Box::new(f(e))),
                special: *special,
                shorthand: *shorthand,
            }
        }
        Expr::Trim { expr: inner, trim_where, trim_what, trim_characters } => {
            Expr::Trim {
                expr: Box::new(f(inner)),
                trim_where: *trim_where,
                trim_what: trim_what.as_ref().map(|e| Box::new(f(e))),
                trim_characters: trim_characters.as_ref().map(|v| v.iter().map(f).collect()),
            }
        }
        Expr::Case { case_token, end_token, operand, conditions, else_result } => {
            Expr::Case {
                case_token: case_token.clone(),
                end_token: end_token.clone(),
                operand: operand.as_ref().map(|e| Box::new(f(e))),
                conditions: conditions
                    .iter()
                    .map(|cw| {
                        sqlparser::ast::CaseWhen {
                            condition: f(&cw.condition),
                            result: f(&cw.result),
                        }
                    })
                    .collect(),
                else_result: else_result.as_ref().map(|e| Box::new(f(e))),
            }
        }
        Expr::InSubquery { expr: inner, subquery, negated } => {
            // NOTE: We only transform the expr child. The subquery is a Query,
            // not an Expr, so callers needing subquery traversal must handle
            // InSubquery/Subquery/Exists themselves.
            Expr::InSubquery {
                expr: Box::new(f(inner)),
                subquery: subquery.clone(),
                negated: *negated,
            }
        }
        Expr::InUnnest { expr: inner, array_expr, negated } => {
            Expr::InUnnest {
                expr: Box::new(f(inner)),
                array_expr: Box::new(f(array_expr)),
                negated: *negated,
            }
        }
        Expr::Interval(interval) => {
            Expr::Interval(sqlparser::ast::Interval {
                value: Box::new(f(&interval.value)),
                leading_field: interval.leading_field.clone(),
                leading_precision: interval.leading_precision,
                last_field: interval.last_field.clone(),
                fractional_seconds_precision: interval.fractional_seconds_precision,
            })
        }

        Expr::CompoundFieldAccess { root, access_chain } => {
            Expr::CompoundFieldAccess {
                root: Box::new(f(root)),
                access_chain: access_chain
                    .iter()
                    .map(|a| {
                        match a {
                            sqlparser::ast::AccessExpr::Dot(e) => {
                                sqlparser::ast::AccessExpr::Dot(f(e))
                            }
                            sqlparser::ast::AccessExpr::Subscript(sub) => {
                                sqlparser::ast::AccessExpr::Subscript(map_subscript(sub, f))
                            }
                        }
                    })
                    .collect(),
            }
        }
        Expr::JsonAccess { value, path } => {
            Expr::JsonAccess {
                value: Box::new(f(value)),
                path: sqlparser::ast::JsonPath {
                    path: path
                        .path
                        .iter()
                        .map(|elem| {
                            match elem {
                                JsonPathElem::Dot { key, quoted } => {
                                    JsonPathElem::Dot { key: key.clone(), quoted: *quoted }
                                }
                                JsonPathElem::Bracket { key } => {
                                    JsonPathElem::Bracket { key: f(key) }
                                }
                                JsonPathElem::ColonBracket { key } => {
                                    JsonPathElem::ColonBracket { key: f(key) }
                                }
                            }
                        })
                        .collect(),
                },
            }
        }

        // Function and subquery nodes are not walked. Callers must handle them.
        Expr::Function(_) | Expr::Subquery(_) | Expr::Exists { .. } => expr.clone(),

        _ => expr.clone(),
    }
}

/// Fallible version of [`map_expr_children`]. `Function` is not walked, so
/// callers must handle it separately (function name rewriting, argument
/// translation, etc.). Also recurses into `Subquery`, `Exists`, and
/// `InSubquery` via `f_query`.
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
pub(crate) fn try_map_expr_children<E>(
    expr: &Expr,
    f: &impl Fn(&Expr) -> Result<Expr, E>,
    f_query: &impl Fn(&sqlparser::ast::Query) -> Result<sqlparser::ast::Query, E>,
) -> Result<Expr, E> {
    Ok(match expr {
        Expr::Identifier(_)
        | Expr::CompoundIdentifier(_)
        | Expr::Value(_)
        | Expr::TypedString(_)
        | Expr::Wildcard(_)
        | Expr::QualifiedWildcard(..)
        | Expr::MatchAgainst { .. } => expr.clone(),

        Expr::IsFalse(e) => Expr::IsFalse(Box::new(f(e)?)),
        Expr::IsNotFalse(e) => Expr::IsNotFalse(Box::new(f(e)?)),
        Expr::IsTrue(e) => Expr::IsTrue(Box::new(f(e)?)),
        Expr::IsNotTrue(e) => Expr::IsNotTrue(Box::new(f(e)?)),
        Expr::IsNull(e) => Expr::IsNull(Box::new(f(e)?)),
        Expr::IsNotNull(e) => Expr::IsNotNull(Box::new(f(e)?)),
        Expr::IsUnknown(e) => Expr::IsUnknown(Box::new(f(e)?)),
        Expr::IsNotUnknown(e) => Expr::IsNotUnknown(Box::new(f(e)?)),
        Expr::Nested(e) => Expr::Nested(Box::new(f(e)?)),
        Expr::OuterJoin(e) => Expr::OuterJoin(Box::new(f(e)?)),
        Expr::Prior(e) => Expr::Prior(Box::new(f(e)?)),
        Expr::Prefixed { prefix, value } => {
            Expr::Prefixed { prefix: prefix.clone(), value: Box::new(f(value)?) }
        }
        Expr::Named { expr: inner, name } => {
            Expr::Named { expr: Box::new(f(inner)?), name: name.clone() }
        }
        Expr::IsNormalized { expr: inner, form, negated } => {
            Expr::IsNormalized { expr: Box::new(f(inner)?), form: *form, negated: *negated }
        }
        Expr::IsJson { expr: inner, kind, unique_keys, negated } => {
            Expr::IsJson {
                expr: Box::new(f(inner)?),
                kind: *kind,
                unique_keys: *unique_keys,
                negated: *negated,
            }
        }
        Expr::UnaryOp { op, expr: inner } => Expr::UnaryOp { op: *op, expr: Box::new(f(inner)?) },
        Expr::Cast { kind, expr: inner, data_type, format } => {
            Expr::Cast {
                kind: kind.clone(),
                expr: Box::new(f(inner)?),
                data_type: data_type.clone(),
                format: format.clone(),
            }
        }
        Expr::Extract { field, syntax, expr: inner } => {
            Expr::Extract {
                field: field.clone(),
                syntax: syntax.clone(),
                expr: Box::new(f(inner)?),
            }
        }
        Expr::Ceil { expr: inner, field } => {
            Expr::Ceil { expr: Box::new(f(inner)?), field: field.clone() }
        }
        Expr::Floor { expr: inner, field } => {
            Expr::Floor { expr: Box::new(f(inner)?), field: field.clone() }
        }
        Expr::Collate { expr: inner, collation } => {
            Expr::Collate { expr: Box::new(f(inner)?), collation: collation.clone() }
        }
        Expr::Convert { is_try, expr: inner, data_type, charset, target_before_value, styles } => {
            Expr::Convert {
                is_try: *is_try,
                expr: Box::new(f(inner)?),
                data_type: data_type.clone(),
                charset: charset.clone(),
                target_before_value: *target_before_value,
                styles: styles.iter().map(f).collect::<Result<_, _>>()?,
            }
        }

        Expr::IsDistinctFrom(a, b) => Expr::IsDistinctFrom(Box::new(f(a)?), Box::new(f(b)?)),
        Expr::IsNotDistinctFrom(a, b) => Expr::IsNotDistinctFrom(Box::new(f(a)?), Box::new(f(b)?)),
        Expr::BinaryOp { left, op, right } => {
            Expr::BinaryOp { left: Box::new(f(left)?), op: op.clone(), right: Box::new(f(right)?) }
        }
        Expr::AnyOp { left, compare_op, right, is_some } => {
            Expr::AnyOp {
                left: Box::new(f(left)?),
                compare_op: compare_op.clone(),
                right: Box::new(f(right)?),
                is_some: *is_some,
            }
        }
        Expr::AllOp { left, compare_op, right } => {
            Expr::AllOp {
                left: Box::new(f(left)?),
                compare_op: compare_op.clone(),
                right: Box::new(f(right)?),
            }
        }
        Expr::Like { negated, any, expr: inner, pattern, escape_char } => {
            Expr::Like {
                negated: *negated,
                any: *any,
                expr: Box::new(f(inner)?),
                pattern: Box::new(f(pattern)?),
                escape_char: escape_char.clone(),
            }
        }
        Expr::ILike { negated, any, expr: inner, pattern, escape_char } => {
            Expr::ILike {
                negated: *negated,
                any: *any,
                expr: Box::new(f(inner)?),
                pattern: Box::new(f(pattern)?),
                escape_char: escape_char.clone(),
            }
        }
        Expr::SimilarTo { negated, expr: inner, pattern, escape_char } => {
            Expr::SimilarTo {
                negated: *negated,
                expr: Box::new(f(inner)?),
                pattern: Box::new(f(pattern)?),
                escape_char: escape_char.clone(),
            }
        }
        Expr::RLike { negated, expr: inner, pattern, regexp } => {
            Expr::RLike {
                negated: *negated,
                expr: Box::new(f(inner)?),
                pattern: Box::new(f(pattern)?),
                regexp: *regexp,
            }
        }
        Expr::AtTimeZone { timestamp, time_zone } => {
            Expr::AtTimeZone {
                timestamp: Box::new(f(timestamp)?),
                time_zone: Box::new(f(time_zone)?),
            }
        }
        Expr::Position { expr: inner, r#in } => {
            Expr::Position { expr: Box::new(f(inner)?), r#in: Box::new(f(r#in)?) }
        }

        Expr::Between { expr: inner, negated, low, high } => {
            Expr::Between {
                expr: Box::new(f(inner)?),
                negated: *negated,
                low: Box::new(f(low)?),
                high: Box::new(f(high)?),
            }
        }
        Expr::Overlay { expr: inner, overlay_what, overlay_from, overlay_for } => {
            Expr::Overlay {
                expr: Box::new(f(inner)?),
                overlay_what: Box::new(f(overlay_what)?),
                overlay_from: Box::new(f(overlay_from)?),
                overlay_for: overlay_for.as_ref().map(|e| f(e)).transpose()?.map(Box::new),
            }
        }

        Expr::InList { expr: inner, list, negated } => {
            Expr::InList {
                expr: Box::new(f(inner)?),
                list: list.iter().map(f).collect::<Result<_, _>>()?,
                negated: *negated,
            }
        }
        Expr::Tuple(items) => Expr::Tuple(items.iter().map(f).collect::<Result<_, _>>()?),
        Expr::Array(arr) => {
            Expr::Array(sqlparser::ast::Array {
                elem: arr.elem.iter().map(f).collect::<Result<_, _>>()?,
                named: arr.named,
            })
        }
        Expr::GroupingSets(sets) => {
            Expr::GroupingSets(
                sets.iter()
                    .map(|s| s.iter().map(f).collect::<Result<_, _>>())
                    .collect::<Result<_, _>>()?,
            )
        }
        Expr::Cube(sets) => {
            Expr::Cube(
                sets.iter()
                    .map(|s| s.iter().map(f).collect::<Result<_, _>>())
                    .collect::<Result<_, _>>()?,
            )
        }
        Expr::Rollup(sets) => {
            Expr::Rollup(
                sets.iter()
                    .map(|s| s.iter().map(f).collect::<Result<_, _>>())
                    .collect::<Result<_, _>>()?,
            )
        }
        Expr::Struct { values, fields } => {
            Expr::Struct {
                values: values.iter().map(f).collect::<Result<_, _>>()?,
                fields: fields.clone(),
            }
        }

        Expr::Substring { expr: inner, substring_from, substring_for, special, shorthand } => {
            Expr::Substring {
                expr: Box::new(f(inner)?),
                substring_from: substring_from.as_ref().map(|e| f(e)).transpose()?.map(Box::new),
                substring_for: substring_for.as_ref().map(|e| f(e)).transpose()?.map(Box::new),
                special: *special,
                shorthand: *shorthand,
            }
        }
        Expr::Trim { expr: inner, trim_where, trim_what, trim_characters } => {
            Expr::Trim {
                expr: Box::new(f(inner)?),
                trim_where: *trim_where,
                trim_what: trim_what.as_ref().map(|e| f(e)).transpose()?.map(Box::new),
                trim_characters: trim_characters
                    .as_ref()
                    .map(|v| v.iter().map(f).collect::<Result<_, _>>())
                    .transpose()?,
            }
        }
        Expr::Case { case_token, end_token, operand, conditions, else_result } => {
            Expr::Case {
                case_token: case_token.clone(),
                end_token: end_token.clone(),
                operand: operand.as_ref().map(|e| f(e)).transpose()?.map(Box::new),
                conditions: conditions
                    .iter()
                    .map(|cw| {
                        Ok(sqlparser::ast::CaseWhen {
                            condition: f(&cw.condition)?,
                            result: f(&cw.result)?,
                        })
                    })
                    .collect::<Result<Vec<_>, E>>()?,
                else_result: else_result.as_ref().map(|e| f(e)).transpose()?.map(Box::new),
            }
        }
        Expr::Interval(interval) => {
            Expr::Interval(sqlparser::ast::Interval {
                value: Box::new(f(&interval.value)?),
                leading_field: interval.leading_field.clone(),
                leading_precision: interval.leading_precision,
                last_field: interval.last_field.clone(),
                fractional_seconds_precision: interval.fractional_seconds_precision,
            })
        }
        Expr::InUnnest { expr: inner, array_expr, negated } => {
            Expr::InUnnest {
                expr: Box::new(f(inner)?),
                array_expr: Box::new(f(array_expr)?),
                negated: *negated,
            }
        }

        Expr::CompoundFieldAccess { root, access_chain } => {
            Expr::CompoundFieldAccess {
                root: Box::new(f(root)?),
                access_chain: access_chain
                    .iter()
                    .map(|a| try_map_access_expr(a, f))
                    .collect::<Result<_, _>>()?,
            }
        }
        Expr::JsonAccess { value, path } => {
            Expr::JsonAccess { value: Box::new(f(value)?), path: try_map_json_path(path, f)? }
        }

        // Subquery and Exists nodes are walked via f_query.
        Expr::Subquery(q) => Expr::Subquery(Box::new(f_query(q)?)),
        Expr::Exists { subquery, negated } => {
            Expr::Exists { subquery: Box::new(f_query(subquery)?), negated: *negated }
        }
        Expr::InSubquery { expr: inner, subquery, negated } => {
            Expr::InSubquery {
                expr: Box::new(f(inner)?),
                subquery: Box::new(f_query(subquery)?),
                negated: *negated,
            }
        }

        // Dictionary, Map, Lambda, and MemberOf recurse into their children.
        Expr::Dictionary(fields) => {
            Expr::Dictionary(
                fields
                    .iter()
                    .map(|field| {
                        Ok(sqlparser::ast::DictionaryField {
                            key: field.key.clone(),
                            value: Box::new(f(&field.value)?),
                        })
                    })
                    .collect::<Result<Vec<_>, E>>()?,
            )
        }
        Expr::Map(map) => {
            Expr::Map(sqlparser::ast::Map {
                entries: map
                    .entries
                    .iter()
                    .map(|entry| {
                        Ok(sqlparser::ast::MapEntry {
                            key: Box::new(f(&entry.key)?),
                            value: Box::new(f(&entry.value)?),
                        })
                    })
                    .collect::<Result<Vec<_>, E>>()?,
            })
        }
        Expr::Lambda(lambda) => {
            Expr::Lambda(sqlparser::ast::LambdaFunction {
                params: lambda.params.clone(),
                body: Box::new(f(&lambda.body)?),
                syntax: lambda.syntax,
            })
        }
        Expr::MemberOf(member) => {
            Expr::MemberOf(sqlparser::ast::MemberOf {
                value: Box::new(f(&member.value)?),
                array: Box::new(f(&member.array)?),
            })
        }

        // Function is not walked. Callers must handle it separately.
        Expr::Function(_) => expr.clone(),
    })
}

fn try_map_access_expr<E>(
    access: &sqlparser::ast::AccessExpr,
    f: &impl Fn(&Expr) -> Result<Expr, E>,
) -> Result<sqlparser::ast::AccessExpr, E> {
    Ok(match access {
        sqlparser::ast::AccessExpr::Dot(e) => sqlparser::ast::AccessExpr::Dot(f(e)?),
        sqlparser::ast::AccessExpr::Subscript(sub) => {
            sqlparser::ast::AccessExpr::Subscript(try_map_subscript(sub, f)?)
        }
    })
}

fn try_map_subscript<E>(
    sub: &sqlparser::ast::Subscript,
    f: &impl Fn(&Expr) -> Result<Expr, E>,
) -> Result<sqlparser::ast::Subscript, E> {
    Ok(match sub {
        sqlparser::ast::Subscript::Index { index } => {
            sqlparser::ast::Subscript::Index { index: f(index)? }
        }
        sqlparser::ast::Subscript::Slice { lower_bound, upper_bound, stride } => {
            sqlparser::ast::Subscript::Slice {
                lower_bound: lower_bound.as_ref().map(f).transpose()?,
                upper_bound: upper_bound.as_ref().map(f).transpose()?,
                stride: stride.as_ref().map(f).transpose()?,
            }
        }
    })
}

fn try_map_json_path<E>(
    path: &sqlparser::ast::JsonPath,
    f: &impl Fn(&Expr) -> Result<Expr, E>,
) -> Result<sqlparser::ast::JsonPath, E> {
    Ok(sqlparser::ast::JsonPath {
        path: path
            .path
            .iter()
            .map(|elem| {
                Ok(match elem {
                    JsonPathElem::Dot { key, quoted } => {
                        JsonPathElem::Dot { key: key.clone(), quoted: *quoted }
                    }
                    JsonPathElem::Bracket { key } => JsonPathElem::Bracket { key: f(key)? },
                    JsonPathElem::ColonBracket { key } => {
                        JsonPathElem::ColonBracket { key: f(key)? }
                    }
                })
            })
            .collect::<Result<Vec<_>, E>>()?,
    })
}

/// Read-only variant of [`map_expr_children`] that does not rebuild the tree.
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
pub(crate) fn for_each_child_expr(expr: &Expr, f: &mut impl FnMut(&Expr)) {
    match expr {
        // leaf nodes
        Expr::Identifier(_)
        | Expr::CompoundIdentifier(_)
        | Expr::Value(_)
        | Expr::TypedString(_)
        | Expr::Wildcard(_)
        | Expr::QualifiedWildcard(..)
        | Expr::MatchAgainst { .. }
        | Expr::Lambda(_)
        | Expr::MemberOf(_) => {}

        // single child
        Expr::IsFalse(e)
        | Expr::IsNotFalse(e)
        | Expr::IsTrue(e)
        | Expr::IsNotTrue(e)
        | Expr::IsNull(e)
        | Expr::IsNotNull(e)
        | Expr::IsUnknown(e)
        | Expr::IsNotUnknown(e)
        | Expr::Nested(e)
        | Expr::OuterJoin(e)
        | Expr::Prior(e) => f(e),

        Expr::Prefixed { value, .. } => f(value),
        Expr::Named { expr: inner, .. }
        | Expr::IsNormalized { expr: inner, .. }
        | Expr::IsJson { expr: inner, .. } => f(inner),
        Expr::UnaryOp { expr: inner, .. }
        | Expr::Cast { expr: inner, .. }
        | Expr::Extract { expr: inner, .. }
        | Expr::Ceil { expr: inner, .. }
        | Expr::Floor { expr: inner, .. }
        | Expr::Collate { expr: inner, .. } => f(inner),
        Expr::Convert { expr: inner, styles, .. } => {
            f(inner);
            for s in styles {
                f(s);
            }
        }

        // two children
        Expr::IsDistinctFrom(a, b) | Expr::IsNotDistinctFrom(a, b) => {
            f(a);
            f(b);
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::AnyOp { left, right, .. }
        | Expr::AllOp { left, right, .. } => {
            f(left);
            f(right);
        }
        Expr::Like { expr: inner, pattern, .. }
        | Expr::ILike { expr: inner, pattern, .. }
        | Expr::SimilarTo { expr: inner, pattern, .. }
        | Expr::RLike { expr: inner, pattern, .. } => {
            f(inner);
            f(pattern);
        }
        Expr::AtTimeZone { timestamp, time_zone } => {
            f(timestamp);
            f(time_zone);
        }
        Expr::Position { expr: inner, r#in } => {
            f(inner);
            f(r#in);
        }

        // three children
        Expr::Between { expr: inner, low, high, .. } => {
            f(inner);
            f(low);
            f(high);
        }
        Expr::Overlay { expr: inner, overlay_what, overlay_from, overlay_for } => {
            f(inner);
            f(overlay_what);
            f(overlay_from);
            if let Some(e) = overlay_for {
                f(e);
            }
        }

        // list children
        Expr::InList { expr: inner, list, .. } => {
            f(inner);
            for e in list {
                f(e);
            }
        }
        Expr::Tuple(items) => {
            for e in items {
                f(e);
            }
        }
        Expr::Array(arr) => {
            for e in &arr.elem {
                f(e);
            }
        }
        Expr::GroupingSets(sets) | Expr::Cube(sets) | Expr::Rollup(sets) => {
            for set in sets {
                for e in set {
                    f(e);
                }
            }
        }
        Expr::Struct { values, .. } => {
            for e in values {
                f(e);
            }
        }

        // structured with optional children
        Expr::Substring { expr: inner, substring_from, substring_for, .. } => {
            f(inner);
            if let Some(e) = substring_from {
                f(e);
            }
            if let Some(e) = substring_for {
                f(e);
            }
        }
        Expr::Trim { expr: inner, trim_what, trim_characters, .. } => {
            f(inner);
            if let Some(e) = trim_what {
                f(e);
            }
            if let Some(chars) = trim_characters {
                for c in chars {
                    f(c);
                }
            }
        }
        Expr::Case { operand, conditions, else_result, .. } => {
            if let Some(e) = operand {
                f(e);
            }
            for cw in conditions {
                f(&cw.condition);
                f(&cw.result);
            }
            if let Some(e) = else_result {
                f(e);
            }
        }
        Expr::InSubquery { expr: inner, .. } => f(inner),
        Expr::InUnnest { expr: inner, array_expr, .. } => {
            f(inner);
            f(array_expr);
        }
        Expr::Interval(interval) => f(&interval.value),

        // compound access
        Expr::CompoundFieldAccess { root, access_chain } => {
            f(root);
            for a in access_chain {
                match a {
                    sqlparser::ast::AccessExpr::Dot(e) => f(e),
                    sqlparser::ast::AccessExpr::Subscript(sub) => {
                        for_each_subscript_expr(sub, f);
                    }
                }
            }
        }
        Expr::JsonAccess { value, path } => {
            f(value);
            for elem in &path.path {
                if let JsonPathElem::Bracket { key } = elem {
                    f(key);
                }
            }
        }

        // Function / Subquery / Exists - skip (callers handle separately)
        Expr::Function(_) | Expr::Subquery(_) | Expr::Exists { .. } => {}

        // Remaining leaf-like variants
        _ => {}
    }
}

/// Return `true` if `predicate` returns `true` for any direct child [`Expr`]
/// inside `expr`.
pub(crate) fn any_child_expr(expr: &Expr, predicate: &impl Fn(&Expr) -> bool) -> bool {
    let mut found = false;
    for_each_child_expr(expr, &mut |child| {
        if !found && predicate(child) {
            found = true;
        }
    });
    found
}

/// Calls `f` on every direct child `&mut Expr`, mutating in place.
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
pub(crate) fn mutate_expr_children(expr: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    match expr {
        // leaf nodes
        Expr::Identifier(_)
        | Expr::CompoundIdentifier(_)
        | Expr::Value(_)
        | Expr::TypedString(_)
        | Expr::Wildcard(_)
        | Expr::QualifiedWildcard(..)
        | Expr::MatchAgainst { .. }
        | Expr::Lambda(_)
        | Expr::MemberOf(_) => {}

        // single child
        Expr::IsFalse(e)
        | Expr::IsNotFalse(e)
        | Expr::IsTrue(e)
        | Expr::IsNotTrue(e)
        | Expr::IsNull(e)
        | Expr::IsNotNull(e)
        | Expr::IsUnknown(e)
        | Expr::IsNotUnknown(e)
        | Expr::Nested(e)
        | Expr::OuterJoin(e)
        | Expr::Prior(e) => f(e),

        Expr::Prefixed { value, .. } => f(value),
        Expr::Named { expr: inner, .. }
        | Expr::IsNormalized { expr: inner, .. }
        | Expr::IsJson { expr: inner, .. } => f(inner),
        Expr::UnaryOp { expr: inner, .. }
        | Expr::Cast { expr: inner, .. }
        | Expr::Extract { expr: inner, .. }
        | Expr::Ceil { expr: inner, .. }
        | Expr::Floor { expr: inner, .. }
        | Expr::Collate { expr: inner, .. } => f(inner),
        Expr::Convert { expr: inner, styles, .. } => {
            f(inner);
            for s in styles {
                f(s);
            }
        }

        // two children
        Expr::IsDistinctFrom(a, b) | Expr::IsNotDistinctFrom(a, b) => {
            f(a);
            f(b);
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::AnyOp { left, right, .. }
        | Expr::AllOp { left, right, .. } => {
            f(left);
            f(right);
        }
        Expr::Like { expr: inner, pattern, .. }
        | Expr::ILike { expr: inner, pattern, .. }
        | Expr::SimilarTo { expr: inner, pattern, .. }
        | Expr::RLike { expr: inner, pattern, .. } => {
            f(inner);
            f(pattern);
        }
        Expr::AtTimeZone { timestamp, time_zone } => {
            f(timestamp);
            f(time_zone);
        }
        Expr::Position { expr: inner, r#in } => {
            f(inner);
            f(r#in);
        }

        // three children
        Expr::Between { expr: inner, low, high, .. } => {
            f(inner);
            f(low);
            f(high);
        }
        Expr::Overlay { expr: inner, overlay_what, overlay_from, overlay_for } => {
            f(inner);
            f(overlay_what);
            f(overlay_from);
            if let Some(e) = overlay_for {
                f(e);
            }
        }

        // list children
        Expr::InList { expr: inner, list, .. } => {
            f(inner);
            for e in list {
                f(e);
            }
        }
        Expr::Tuple(items) => {
            for e in items {
                f(e);
            }
        }
        Expr::Array(arr) => {
            for e in &mut arr.elem {
                f(e);
            }
        }
        Expr::GroupingSets(sets) | Expr::Cube(sets) | Expr::Rollup(sets) => {
            for set in sets {
                for e in set {
                    f(e);
                }
            }
        }
        Expr::Struct { values, .. } => {
            for e in values {
                f(e);
            }
        }

        // structured with optional children
        Expr::Substring { expr: inner, substring_from, substring_for, .. } => {
            f(inner);
            if let Some(e) = substring_from {
                f(e);
            }
            if let Some(e) = substring_for {
                f(e);
            }
        }
        Expr::Trim { expr: inner, trim_what, trim_characters, .. } => {
            f(inner);
            if let Some(e) = trim_what {
                f(e);
            }
            if let Some(chars) = trim_characters {
                for c in chars {
                    f(c);
                }
            }
        }
        Expr::Case { operand, conditions, else_result, .. } => {
            if let Some(e) = operand {
                f(e);
            }
            for cw in conditions {
                f(&mut cw.condition);
                f(&mut cw.result);
            }
            if let Some(e) = else_result {
                f(e);
            }
        }
        Expr::InSubquery { expr: inner, .. } => f(inner),
        Expr::InUnnest { expr: inner, array_expr, .. } => {
            f(inner);
            f(array_expr);
        }
        Expr::Interval(interval) => f(&mut interval.value),

        // compound access
        Expr::CompoundFieldAccess { root, access_chain } => {
            f(root);
            for a in access_chain {
                match a {
                    sqlparser::ast::AccessExpr::Dot(e) => f(e),
                    sqlparser::ast::AccessExpr::Subscript(sub) => {
                        mutate_subscript_expr(sub, f);
                    }
                }
            }
        }
        Expr::JsonAccess { value, path } => {
            f(value);
            for elem in &mut path.path {
                if let JsonPathElem::Bracket { key } = elem {
                    f(key);
                }
            }
        }

        // Function / Subquery / Exists - skip (callers handle separately)
        Expr::Function(_) | Expr::Subquery(_) | Expr::Exists { .. } => {}

        // Remaining leaf-like variants
        _ => {}
    }
}

fn map_subscript(
    sub: &sqlparser::ast::Subscript,
    f: &impl Fn(&Expr) -> Expr,
) -> sqlparser::ast::Subscript {
    match sub {
        sqlparser::ast::Subscript::Index { index } => {
            sqlparser::ast::Subscript::Index { index: f(index) }
        }
        sqlparser::ast::Subscript::Slice { lower_bound, upper_bound, stride } => {
            sqlparser::ast::Subscript::Slice {
                lower_bound: lower_bound.as_ref().map(f),
                upper_bound: upper_bound.as_ref().map(f),
                stride: stride.as_ref().map(f),
            }
        }
    }
}

fn for_each_subscript_expr(sub: &sqlparser::ast::Subscript, f: &mut impl FnMut(&Expr)) {
    match sub {
        sqlparser::ast::Subscript::Index { index } => f(index),
        sqlparser::ast::Subscript::Slice { lower_bound, upper_bound, stride } => {
            if let Some(e) = lower_bound {
                f(e);
            }
            if let Some(e) = upper_bound {
                f(e);
            }
            if let Some(e) = stride {
                f(e);
            }
        }
    }
}

fn mutate_subscript_expr(sub: &mut sqlparser::ast::Subscript, f: &mut impl FnMut(&mut Expr)) {
    match sub {
        sqlparser::ast::Subscript::Index { index } => f(index),
        sqlparser::ast::Subscript::Slice { lower_bound, upper_bound, stride } => {
            if let Some(e) = lower_bound {
                f(e);
            }
            if let Some(e) = upper_bound {
                f(e);
            }
            if let Some(e) = stride {
                f(e);
            }
        }
    }
}

/// `CASE WHEN <condition> THEN <then_expr> [ELSE <else_expr>] END`.
///
/// Omitting `else_expr` yields NULL when the condition is false or NULL, which
/// is how the translators express a guarded value.
#[must_use]
pub(crate) fn case_when(condition: Expr, then_expr: Expr, else_expr: Option<Expr>) -> Expr {
    Expr::Case {
        case_token: AttachedToken::empty(),
        end_token: AttachedToken::empty(),
        operand: None,
        conditions: vec![CaseWhen { condition, result: then_expr }],
        else_result: else_expr.map(Box::new),
    }
}

/// `<left> IS <right>`, SQLite's null-safe equality. PostgreSQL spells the
/// same thing `IS NOT DISTINCT FROM`, which SQLite does not accept, and
/// `BinaryOperator::Custom` is the only way to render the bare `IS`.
#[must_use]
pub(crate) fn null_safe_eq(left: Expr, right: Expr) -> Expr {
    Expr::BinaryOp {
        left: Box::new(left),
        op: sqlparser::ast::BinaryOperator::Custom("IS".to_string()),
        right: Box::new(right),
    }
}

/// `NOT (<expr>)`, parenthesized so the negation binds the whole predicate
/// rather than its leftmost operand.
#[must_use]
pub(crate) fn not_predicate(expr: Expr) -> Expr {
    Expr::UnaryOp {
        op: sqlparser::ast::UnaryOperator::Not,
        expr: Box::new(Expr::Nested(Box::new(expr))),
    }
}

/// `NOT (<left> IS <right>)`, SQLite's null-safe inequality.
#[must_use]
pub(crate) fn null_safe_neq(left: Expr, right: Expr) -> Expr {
    not_predicate(null_safe_eq(left, right))
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sqlparser::ast::{Expr, Ident, Value, ValueWithSpan};

    use super::*;

    fn ident_expr(name: &str) -> Expr {
        Expr::Identifier(Ident::new(name))
    }

    fn num_expr(n: &str) -> Expr {
        Expr::Value(ValueWithSpan {
            value: Value::Number(n.to_string(), false),
            span: sqlparser::tokenizer::Span::empty(),
        })
    }

    #[test]
    fn map_expr_children_transforms_binary_op() {
        let expr = Expr::BinaryOp {
            left: Box::new(ident_expr("a")),
            op: sqlparser::ast::BinaryOperator::Plus,
            right: Box::new(num_expr("1")),
        };
        // Wrap every child in Nested
        let result = map_expr_children(&expr, &|e| Expr::Nested(Box::new(e.clone())));
        assert_eq!(result.to_string(), "(a) + (1)");
    }

    #[test]
    fn map_expr_children_leaves_leaf_unchanged() {
        let expr = ident_expr("x");
        let result = map_expr_children(&expr, &|_| panic!("should not be called on leaf"));
        assert_eq!(result.to_string(), "x");
    }

    #[test]
    fn for_each_child_expr_visits_case_parts() {
        let expr = Expr::Case {
            case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
            end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
            operand: Some(Box::new(ident_expr("x"))),
            conditions: vec![sqlparser::ast::CaseWhen {
                condition: ident_expr("a"),
                result: ident_expr("b"),
            }],
            else_result: Some(Box::new(ident_expr("c"))),
        };
        let mut visited = Vec::new();
        for_each_child_expr(&expr, &mut |e| visited.push(e.to_string()));
        assert_eq!(visited, vec!["x", "a", "b", "c"]);
    }

    #[test]
    fn any_child_expr_finds_match() {
        let expr = Expr::Between {
            expr: Box::new(ident_expr("x")),
            negated: false,
            low: Box::new(num_expr("1")),
            high: Box::new(ident_expr("target")),
        };
        assert!(any_child_expr(&expr, &|e| e.to_string() == "target"));
        assert!(!any_child_expr(&expr, &|e| e.to_string() == "missing"));
    }

    #[test]
    fn mutate_expr_children_transforms_in_place() {
        let mut expr = Expr::Tuple(vec![ident_expr("a"), ident_expr("b")]);
        mutate_expr_children(&mut expr, &mut |e| {
            if let Expr::Identifier(ident) = e {
                ident.value = ident.value.to_uppercase();
            }
        });
        assert_eq!(expr.to_string(), "(A, B)");
    }

    /// Parse a single SQL expression using the PostgreSQL dialect.
    fn parse_expr(sql: &str) -> Expr {
        let full = format!("SELECT {sql} FROM dummy");
        let dialect = sqlparser::dialect::PostgreSqlDialect {};
        let mut stmts = sqlparser::parser::Parser::parse_sql(&dialect, &full)
            .unwrap_or_else(|e| panic!("parse failed for `{sql}`: {e}"));
        let stmt = stmts.pop().expect("statement");
        let sqlparser::ast::Statement::Query(query) = stmt else { panic!("not a query") };
        let sqlparser::ast::SetExpr::Select(select) = *query.body else { panic!("not a select") };
        let projection = select.projection.into_iter().next().expect("projection");
        match projection {
            sqlparser::ast::SelectItem::UnnamedExpr(e)
            | sqlparser::ast::SelectItem::ExprWithAlias { expr: e, .. } => e,
            other => panic!("non-Expr projection for `{sql}`: {other:?}"),
        }
    }

    /// SQL expressions that produce a wide spread of `Expr` variants. Used
    /// below to exercise every walker uniformly.
    fn sample_expressions() -> Vec<(&'static str, &'static str)> {
        vec![
            // Leaves and value-like variants
            ("identifier", "a"),
            ("compound_identifier", "schema.tbl.col"),
            ("number_value", "1"),
            ("string_value", "'hi'"),
            ("typed_string", "TIMESTAMP '2020-01-01'"),
            // Single-child wrappers
            ("is_false", "a IS FALSE"),
            ("is_not_false", "a IS NOT FALSE"),
            ("is_true", "a IS TRUE"),
            ("is_not_true", "a IS NOT TRUE"),
            ("is_null", "a IS NULL"),
            ("is_not_null", "a IS NOT NULL"),
            ("is_unknown", "a IS UNKNOWN"),
            ("is_not_unknown", "a IS NOT UNKNOWN"),
            ("is_normalized", "a IS NORMALIZED"),
            ("is_json", "a IS JSON"),
            ("is_not_json", "a IS NOT JSON"),
            ("is_json_array", "a IS JSON ARRAY"),
            ("is_json_unique_keys", "a IS JSON OBJECT WITH UNIQUE KEYS"),
            ("nested", "(a + 1)"),
            ("unary_op_not", "NOT a"),
            ("unary_op_minus", "-a"),
            ("cast", "CAST(a AS INTEGER)"),
            ("try_cast", "TRY_CAST(a AS INTEGER)"),
            ("extract", "EXTRACT(YEAR FROM a)"),
            ("ceil_scale", "CEIL(a)"),
            ("floor_scale", "FLOOR(a)"),
            ("collate", "a COLLATE \"C\""),
            ("convert", "CONVERT(a USING utf8)"),
            // Two-child operators
            ("binary_op_plus", "a + b"),
            ("binary_op_eq", "a = b"),
            ("binary_op_and", "a AND b"),
            ("any_op", "a = ANY(b)"),
            ("all_op", "a = ALL(b)"),
            ("like", "a LIKE 'x%'"),
            ("ilike", "a ILIKE 'x%'"),
            ("similar_to", "a SIMILAR TO 'x%'"),
            ("position", "POSITION('x' IN a)"),
            ("at_time_zone", "a AT TIME ZONE 'UTC'"),
            ("is_distinct_from", "a IS DISTINCT FROM b"),
            ("is_not_distinct_from", "a IS NOT DISTINCT FROM b"),
            // Lists, ranges, structured
            ("tuple", "(a, b, c)"),
            ("array_value", "ARRAY[a, b, c]"),
            ("in_list", "a IN (1, 2, 3)"),
            ("in_subquery", "a IN (SELECT id FROM t)"),
            ("between", "a BETWEEN 1 AND 10"),
            ("case_with_operand", "CASE a WHEN 1 THEN 'one' ELSE 'other' END"),
            ("case_searched", "CASE WHEN a > 0 THEN 'pos' END"),
            ("trim_chars", "TRIM(BOTH 'x' FROM a)"),
            ("substring", "SUBSTRING(a FROM 1 FOR 3)"),
            ("overlay", "OVERLAY(a PLACING 'z' FROM 2 FOR 1)"),
            ("compound_field_access", "a.b.c"),
            ("interval", "INTERVAL '1 day'"),
            ("subquery", "(SELECT max(id) FROM t)"),
            ("exists", "EXISTS (SELECT 1 FROM t)"),
            ("subscript", "a[1]"),
            ("subscript_slice", "a[1:3]"),
            ("function", "now()"),
            ("function_with_args", "concat(a, b, c)"),
            ("json_access_arrow", "a -> 'k'"),
            ("json_access_long_arrow", "a -> 'k' ->> 'v'"),
        ]
    }

    /// Smoke-test that every walker handles every sampled `Expr` variant
    /// without panicking and that `map_expr_children` with an identity
    /// transform plus `mutate_expr_children` with a no-op are observably
    /// no-ops. This is what lifts the per-variant arms above the
    /// "never executed" baseline.
    #[test]
    fn walkers_handle_all_sampled_variants() {
        for (label, sql) in sample_expressions() {
            let expr = parse_expr(sql);

            // map_expr_children with identity = same Display
            let mapped = map_expr_children(&expr, &|e| e.clone());
            assert_eq!(
                mapped.to_string(),
                expr.to_string(),
                "{label}: map_expr_children identity changed Display",
            );

            // try_map_expr_children with identity = same Display
            let tried: Result<Expr, ()> =
                try_map_expr_children(&expr, &|e| Ok(e.clone()), &|q| Ok(q.clone()));
            assert_eq!(
                tried.expect("identity should not fail").to_string(),
                expr.to_string(),
                "{label}: try_map_expr_children identity changed Display",
            );

            // for_each_child_expr does not panic
            let mut count = 0;
            for_each_child_expr(&expr, &mut |_| count += 1);

            // any_child_expr with always-false returns false (unless leaf has
            // no children, in which case it also returns false). Always-true
            // is true iff there is at least one child.
            let any_true = any_child_expr(&expr, &|_| true);
            assert_eq!(
                any_true,
                count > 0,
                "{label}: any_child_expr(true) disagrees with for_each_child_expr count",
            );
            assert!(
                !any_child_expr(&expr, &|_| false),
                "{label}: any_child_expr(false) returned true",
            );

            // mutate_expr_children with no-op = same Display
            let mut mutated = expr.clone();
            mutate_expr_children(&mut mutated, &mut |_| {});
            assert_eq!(
                mutated.to_string(),
                expr.to_string(),
                "{label}: mutate_expr_children no-op changed Display",
            );
        }
    }

    #[test]
    fn try_map_expr_children_propagates_error() {
        let expr = Expr::BinaryOp {
            left: Box::new(ident_expr("a")),
            op: sqlparser::ast::BinaryOperator::Plus,
            right: Box::new(num_expr("1")),
        };
        let result: Result<Expr, &'static str> =
            try_map_expr_children(&expr, &|_| Err("boom"), &|q| Ok(q.clone()));
        assert_eq!(result, Err("boom"));
    }

    #[test]
    fn for_each_child_expr_counts_in_list() {
        let expr = parse_expr("a IN (1, 2, 3)");
        let mut count = 0;
        for_each_child_expr(&expr, &mut |_| count += 1);
        // Expected: 1 expr (a) + 3 list items
        assert_eq!(count, 4);
    }

    #[test]
    fn for_each_child_expr_counts_function_args_skip() {
        // Function children are intentionally skipped by for_each_child_expr;
        // callers handle the function name + args separately. The Display
        // here just exercises the Function arm.
        let expr = parse_expr("coalesce(a, b)");
        let mut count = 0;
        for_each_child_expr(&expr, &mut |_| count += 1);
        assert_eq!(count, 0, "function children should be skipped");
    }
}
