//! Shared expression walker/visitor helpers.
//!
//! These helpers capture the structural recursion over [`Expr`] variants so
//! that each specific walker only needs to handle its "interesting" arms and
//! can delegate the mechanical child-traversal to one of these functions.

use sqlparser::ast::{Expr, JsonPathElem};

// ---------------------------------------------------------------------------
// map_expr_children  –  immutable rebuild
// ---------------------------------------------------------------------------

/// Apply `f` to every direct child [`Expr`] inside `expr` and rebuild the
/// parent node with the transformed children.  Non-expression fields (operator
/// tokens, identifiers, data-types, etc.) are cloned unchanged.
///
/// Callers should match their "interesting" variants first and fall through to
/// this function for the remaining boilerplate variants.
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
pub(crate) fn map_expr_children(expr: &Expr, f: &impl Fn(&Expr) -> Expr) -> Expr {
    match expr {
        // -- leaf nodes (no child Expr) → clone as-is -----------------------
        Expr::Identifier(_)
        | Expr::CompoundIdentifier(_)
        | Expr::Value(_)
        | Expr::TypedString(_)
        | Expr::Wildcard(_)
        | Expr::QualifiedWildcard(..)
        | Expr::MatchAgainst { .. }
        | Expr::Lambda(_)
        | Expr::MemberOf(_) => expr.clone(),

        // -- single child ---------------------------------------------------
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
        Expr::UnaryOp { op, expr: inner } => Expr::UnaryOp { op: *op, expr: Box::new(f(inner)) },
        Expr::Cast { kind, expr: inner, data_type, array, format } => {
            Expr::Cast {
                kind: kind.clone(),
                expr: Box::new(f(inner)),
                data_type: data_type.clone(),
                array: *array,
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

        // -- two children ---------------------------------------------------
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

        // -- three children -------------------------------------------------
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

        // -- list children --------------------------------------------------
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

        // -- structured with optional children ------------------------------
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

        // -- compound access ------------------------------------------------
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
                            }
                        })
                        .collect(),
                },
            }
        }

        // -- Function and subquery nodes are NOT walked here. ---------------
        // Callers typically need custom logic for these (translating args,
        // injecting CTEs, rewriting names, etc.), so we just clone.
        Expr::Function(_) | Expr::Subquery(_) | Expr::Exists { .. } => expr.clone(),

        // -- Remaining variants without child exprs (Dictionary, Map, etc.) -
        _ => expr.clone(),
    }
}

// ---------------------------------------------------------------------------
// for_each_child_expr  –  read-only traversal
// ---------------------------------------------------------------------------

/// Call `f` on every direct child [`Expr`] inside `expr`.
///
/// This is the read-only equivalent of [`map_expr_children`]; it does not
/// rebuild the tree.  Useful for collecting information (e.g. variable
/// patterns).
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
        Expr::Named { expr: inner, .. } | Expr::IsNormalized { expr: inner, .. } => f(inner),
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

        // Function / Subquery / Exists – skip (callers handle separately)
        Expr::Function(_) | Expr::Subquery(_) | Expr::Exists { .. } => {}

        // Remaining leaf-like variants
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// any_child_expr  –  read-only predicate
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// mutate_expr_children  –  in-place mutation
// ---------------------------------------------------------------------------

/// Call `f` on every direct child `&mut Expr` inside `expr`, allowing in-place
/// mutation.  Non-expression fields are left untouched.
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
        Expr::Named { expr: inner, .. } | Expr::IsNormalized { expr: inner, .. } => f(inner),
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

        // Function / Subquery / Exists – skip (callers handle separately)
        Expr::Function(_) | Expr::Subquery(_) | Expr::Exists { .. } => {}

        // Remaining leaf-like variants
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Subscript helpers
// ---------------------------------------------------------------------------

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

#[cfg(test)]
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
}
