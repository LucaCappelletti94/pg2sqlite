//! Implementation of the [`ReverseTranslator`] trait for the
//! `Expr` type.

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
    ast::{
        BinaryOperator, CastKind, DataType, Expr, FunctionArg, FunctionArgExpr, FunctionArguments,
        Value, ValueWithSpan,
    },
    tokenizer::Span,
};

use super::{function::reverse_translate_function, helpers::Reverse};
use crate::{
    errors::Error,
    impls::{
        function_helpers::simple_function_expr, shared_helpers::translate_expr_recursive,
        translator_impls::expr::wrap_with_lower,
    },
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

/// The argument of the `lower()` call the forward direction wraps each side of
/// an `ILIKE` in, or `None` for anything else.
///
/// Rebuilding the call through the emitter and comparing is what keeps the two
/// directions in step: a `lower` carrying a window, a filter, a modifier, or a
/// different spelling is not what the rewrite emits, so it is not restored.
fn forward_lower_argument(expr: &Expr) -> Option<&Expr> {
    let Expr::Function(function) = expr else {
        return None;
    };
    let FunctionArguments::List(list) = &function.args else {
        return None;
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(argument))] = list.args.as_slice() else {
        return None;
    };
    (wrap_with_lower(argument.clone()) == *expr).then_some(argument)
}

/// Convert a SQLite GLOB pattern to a PostgreSQL LIKE pattern.
///
/// Returns the transformed pattern string and a flag indicating whether an
/// `ESCAPE '\'` clause is needed (i.e., the original pattern contained a
/// literal `%`, `_`, or `\`).
///
/// Returns an error when the pattern contains a character class (`[`) because
/// LIKE has no equivalent construct.
fn glob_pattern_to_like(pattern: &str) -> Result<(String, bool), Error> {
    if pattern.contains('[') {
        return Err(Error::UnsupportedSQLiteFeature(
            "GLOB character class (e.g. [abc]) has no LIKE equivalent; \
             rewrite using separate LIKE patterns instead"
                .to_string(),
        ));
    }
    let mut result = String::with_capacity(pattern.len() + 4);
    let mut needs_escape = false;
    for ch in pattern.chars() {
        match ch {
            // Backslash must be doubled so it remains a literal when \% / \_
            // use it as the escape character.
            '\\' => {
                result.push('\\');
                result.push('\\');
                needs_escape = true;
            }
            // LIKE wildcards that are literal in GLOB must be escaped.
            '%' => {
                result.push('\\');
                result.push('%');
                needs_escape = true;
            }
            '_' => {
                result.push('\\');
                result.push('_');
                needs_escape = true;
            }
            // GLOB wildcards map to their LIKE equivalents.
            '*' => result.push('%'),
            '?' => result.push('_'),
            other => result.push(other),
        }
    }
    Ok((result, needs_escape))
}

/// Translate `expr GLOB pattern` to `expr LIKE pattern ESCAPE '\'`.
///
/// Only string literal patterns can be converted at translation time. A
/// non-literal pattern is rejected because its contents are unknown.
fn translate_glob_to_like(
    left: &Expr,
    right: &Expr,
    negated: bool,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    let Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(glob_pat), .. }) = right
    else {
        return Err(Error::UnsupportedSQLiteFeature(
            "GLOB with a non-literal pattern cannot be converted to LIKE at translation time; \
             bind the constant pattern before translation"
                .to_string(),
        ));
    };
    let (like_pat, needs_escape) = glob_pattern_to_like(glob_pat)?;
    let translated_expr = ReverseTranslator::reverse_translate(left, schema, options)?;
    let escape_char = if needs_escape {
        Some(ValueWithSpan {
            value: Value::SingleQuotedString("\\".to_string()),
            span: Span::empty(),
        })
    } else {
        None
    };
    Ok(Expr::Like {
        negated,
        any: false,
        expr: Box::new(translated_expr),
        pattern: Box::new(Expr::Value(ValueWithSpan {
            value: Value::SingleQuotedString(like_pat),
            span: Span::empty(),
        })),
        escape_char,
    })
}

/// Return true when `expr` matches the exact shape emitted by the forward
/// translator for PostgreSQL's `random()`:
/// `(CAST(random() AS REAL) + 9223372036854775808.0) / 18446744073709551616.0`.
fn is_forward_random_pattern(expr: &Expr) -> bool {
    let Expr::BinaryOp { left, op: BinaryOperator::Divide, right } = expr else {
        return false;
    };
    // Outer divisor must be the normalization constant.
    let Expr::Value(ValueWithSpan { value: Value::Number(divisor, _), .. }) = right.as_ref() else {
        return false;
    };
    if divisor != "18446744073709551616.0" {
        return false;
    }
    // Left side must be a parenthesized addition.
    let Expr::Nested(inner) = left.as_ref() else {
        return false;
    };
    let Expr::BinaryOp { left: cast_expr, op: BinaryOperator::Plus, right: addend } =
        inner.as_ref()
    else {
        return false;
    };
    // Addend must be the shift constant.
    let Expr::Value(ValueWithSpan { value: Value::Number(shift, _), .. }) = addend.as_ref() else {
        return false;
    };
    if shift != "9223372036854775808.0" {
        return false;
    }
    // Cast must be CAST(random() AS REAL).
    let Expr::Cast { expr: inner_expr, data_type: DataType::Real, kind: CastKind::Cast, .. } =
        cast_expr.as_ref()
    else {
        return false;
    };
    // Inner expression must be random().
    let Expr::Function(func) = inner_expr.as_ref() else {
        return false;
    };
    let name = func.name.0.last().and_then(|p| p.as_ident()).map(|i| i.value.to_ascii_lowercase());
    name.as_deref() == Some("random")
        && matches!(&func.args, FunctionArguments::List(l) if l.args.is_empty())
}

impl ReverseTranslator for Expr {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Self;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        // Intercept the forward-translated random() pattern before the recursive
        // descent would try to reverse-translate random() inside it (and reject it).
        if is_forward_random_pattern(self) {
            return Ok(simple_function_expr("random", vec![], None));
        }

        match self {
            Expr::Function(func) => reverse_translate_function(func, schema, options),

            // SQLite's FTS5 MATCH operator (`table MATCH 'query'`) has no
            // direct PostgreSQL equivalent. Reject it so callers know to
            // rewrite using `to_tsvector(col) @@ to_tsquery(query)` instead.
            Expr::BinaryOp { op: BinaryOperator::Match, left, .. } => {
                Err(Error::UnsupportedSQLiteFeature(format!(
                    "SQLite FTS5 MATCH expression against {left} has no PostgreSQL operator. \
                     Rewrite using to_tsvector(col) @@ to_tsquery(query) instead."
                )))
            }

            // GLOB is case-sensitive globbing. Convert literal patterns to LIKE.
            // A character class (e.g. [abc]) or a non-literal pattern is rejected.
            Expr::BinaryOp { op: BinaryOperator::Custom(op_name), left, right }
                if op_name == "GLOB" =>
            {
                translate_glob_to_like(left, right, false, schema, options)
            }

            // SQLite REGEXP and PostgreSQL ~ are both case-sensitive and agree
            // on common patterns, so this is a rewrite rather than a
            // rejection. Beyond that the two regex dialects diverge and the
            // difference is the caller's to resolve.
            //
            // sqlparser gives the two spellings different nodes: `c REGEXP 'p'`
            // is a BinaryOp, `c NOT REGEXP 'p'` is an RLike.
            Expr::BinaryOp { op: BinaryOperator::Regexp, left, right } => {
                posix_regex(left, right, false, schema, options)
            }

            // SQLite's parser rejects RLIKE outright, so it cannot have come
            // from a SQLite replica.
            Expr::RLike { negated, expr, pattern, regexp } => {
                if *regexp {
                    posix_regex(expr, pattern, *negated, schema, options)
                } else {
                    let not = if *negated { "NOT " } else { "" };
                    Err(Error::UnsupportedSQLiteFeature(format!(
                        "RLIKE is not SQLite syntax, so {expr} {not}RLIKE {pattern} cannot have \
                         come from SQLite. Write REGEXP instead."
                    )))
                }
            }

            // SQLite's implicit rowid column does not exist in PostgreSQL.
            Expr::Identifier(ident) if ident.value.eq_ignore_ascii_case("rowid") => {
                Err(Error::UnsupportedSQLiteFeature(
                    "rowid: SQLite's implicit rowid column does not exist in PostgreSQL; \
                     use an explicit INTEGER PRIMARY KEY column instead"
                        .to_string(),
                ))
            }

            // The forward direction lowers both sides of ILIKE, so
            // `lower(x) LIKE lower(y)` restores to `x ILIKE y`. Measured on
            // PostgreSQL 16, the two readings agree, including on non-ASCII
            // case and on patterns holding wildcards.
            //
            // Only without ESCAPE. `lower()` folds a letter escape character,
            // and the two readings then disagree: with ESCAPE 'X',
            // `'aXbc' ILIKE 'aXb_'` is false while the lowered form is true.
            Expr::Like { negated, any, expr, pattern, escape_char: None } => {
                match (forward_lower_argument(expr), forward_lower_argument(pattern)) {
                    (Some(subject), Some(target)) => {
                        Ok(Expr::ILike {
                            negated: *negated,
                            any: *any,
                            expr: Box::new(subject.reverse_translate(schema, options)?),
                            pattern: Box::new(target.reverse_translate(schema, options)?),
                            escape_char: None,
                        })
                    }
                    _ => translate_expr_recursive::<Reverse>(self, schema, options),
                }
            }

            _ => translate_expr_recursive::<Reverse>(self, schema, options),
        }
    }
}

/// Builds PostgreSQL's POSIX regex match, `~` or `!~`.
fn posix_regex(
    expr: &Expr,
    pattern: &Expr,
    negated: bool,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    Ok(Expr::BinaryOp {
        left: Box::new(Expr::reverse_translate(expr, schema, options)?),
        op: if negated { BinaryOperator::PGRegexNotMatch } else { BinaryOperator::PGRegexMatch },
        right: Box::new(Expr::reverse_translate(pattern, schema, options)?),
    })
}
