//! Shared helper functions for reverse translation of table references,
//! joins, and select items.

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

use sql_traits::structs::ParserDB;
use sqlparser::{
    ast::{Expr, Query, TableWithJoins, UnaryOperator, Value, ValueWithSpan, WindowType},
    tokenizer::Span,
};

use crate::{
    errors::Error,
    impls::{direction_wrappers::define_direction_wrappers, shared_helpers::TranslationDirection},
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

/// Reverse (SQLite → PostgreSQL) translation direction.
pub(crate) struct Reverse;

impl TranslationDirection for Reverse {
    type Options<'a> = crate::options::TranslationContext<'a>;

    fn cte_clause<'options>(
        options: &'options Self::Options<'_>,
    ) -> Option<&'options sqlparser::ast::With> {
        options.cte_clause()
    }

    fn with_scope<'scope>(
        options: &'scope Self::Options<'_>,
        scope: &'scope sql_traits::structs::ColumnScope<
            'scope,
            'scope,
            sql_traits::structs::ParserDB,
        >,
    ) -> Self::Options<'scope> {
        options.with_scope(scope)
    }
    fn config<'options>(options: &'options Self::Options<'_>) -> &'options Pg2SqliteOptions {
        options
    }

    fn translate_expr(
        expr: &Expr,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        _emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Expr, Error> {
        expr.reverse_translate(schema, options)
    }

    fn translate_query(
        query: &Query,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        _emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Query, Error> {
        query.reverse_translate(schema, options)
    }

    fn translate_insert(
        insert: &sqlparser::ast::Insert,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        _emit: crate::warnings::WarningSink<'_>,
    ) -> Result<sqlparser::ast::Insert, Error> {
        insert.reverse_translate(schema, options)
    }

    fn translate_delete(
        delete: &sqlparser::ast::Delete,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        _emit: crate::warnings::WarningSink<'_>,
    ) -> Result<sqlparser::ast::Delete, Error> {
        delete.reverse_translate(schema, options)
    }
}

/// Convert an integer literal held as minor units back to its decimal
/// representation at `scale`.
///
/// The replica stores 1.50 as 150 for a NUMERIC(10,2) column, so 150 at
/// scale 2 becomes 1.50. Digits are shifted, not divided as a float, so
/// 101 at scale 2 is exactly 1.01. Returns `None` when the expression is
/// not a plain integer literal (decimal point, exponent, or a non-value
/// node): the caller should reverse-translate it normally.
pub(crate) fn unscale_integer_literal(expr: &Expr, scale: u32) -> Option<Expr> {
    let (negated, digits) = match expr {
        Expr::Value(ValueWithSpan { value: Value::Number(digits, _), .. }) => (false, digits),
        Expr::UnaryOp { op: op @ (UnaryOperator::Minus | UnaryOperator::Plus), expr: inner } => {
            match inner.as_ref() {
                Expr::Value(ValueWithSpan { value: Value::Number(digits, _), .. }) => {
                    (matches!(op, UnaryOperator::Minus), digits)
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    // A decimal point or exponent means it is not a plain integer.
    if digits.contains('.') || digits.contains(['e', 'E']) {
        return None;
    }
    let minor_units: u128 = digits.parse().ok()?;
    let divisor: u128 = 10_u128.pow(scale);
    let int_part = minor_units / divisor;
    let frac_part = minor_units % divisor;
    let scale_usize = usize::try_from(scale).unwrap_or(38);
    let frac_str = format!("{frac_part:0>scale_usize$}");
    let sign = if negated && (int_part > 0 || frac_part > 0) { "-" } else { "" };
    Some(Expr::Value(ValueWithSpan {
        value: Value::Number(format!("{sign}{int_part}.{frac_str}"), false),
        span: Span::empty(),
    }))
}

define_direction_wrappers! {
    direction = Reverse;
    fn reverse_translate_table_with_joins(table_with_joins: &TableWithJoins) -> TableWithJoins = translate_table_with_joins;
    fn reverse_translate_window_type(over: Option<&WindowType>) -> Option<WindowType> = translate_window_type;
}
