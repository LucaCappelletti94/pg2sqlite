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
    ast::{BinaryOperator, Expr, ObjectNamePart, Value, ValueWithSpan},
    tokenizer::Span,
};

use super::{function::reverse_translate_function, helpers::Reverse};
use crate::{
    errors::Error,
    impls::{
        function_helpers::{simple_function_expr, single_quoted_literal},
        idioms::{ascii_code_point_argument, forward_lower_argument, is_uniform_random_float},
        shared_helpers::translate_expr_recursive,
        temporal_arithmetic::reverse_temporal_arithmetic,
    },
    prelude::ReverseTranslator,
};

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
        return Err(Error::reverse_refusal(
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
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    let Some(glob_pat) = single_quoted_literal(right) else {
        return Err(Error::reverse_refusal(
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

/// True when `escape` names a backslash, the character PostgreSQL's `LIKE`
/// escapes with when the statement names none, and so the one the forward
/// direction attaches to every `LIKE` it emits.
fn is_backslash_escape(escape: &ValueWithSpan) -> bool {
    matches!(&escape.value, Value::SingleQuotedString(character) if character == "\\")
}

impl ReverseTranslator for Expr {
    type Schema = ParserDB;
    type PostgresEntry = Self;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<Self::PostgresEntry, Error> {
        // Intercept the forward-translated random() pattern before the
        // recursive descent would try to reverse-translate random()
        // inside it (and reject it).
        if is_uniform_random_float(self) {
            return Ok(simple_function_expr("random", vec![], None));
        }

        // The forward direction lowers ascii() onto a CASE over unicode(), so
        // the exact shape restores to ascii() rather than reversing its
        // pieces, which would answer NULL for the empty string.
        if let Some(argument) = ascii_code_point_argument(self) {
            return Ok(simple_function_expr(
                "ascii",
                vec![argument.reverse_translate(schema, options)?],
                None,
            ));
        }

        // Same, for the date arithmetic the forward direction lowers onto
        // julianday and unixepoch: those two functions have no PostgreSQL name
        // of their own and would be rejected one at a time.
        if let Some(restored) = reverse_temporal_arithmetic(self, schema, options) {
            return restored;
        }

        match self {
            Expr::Function(func) => reverse_translate_function(func, schema, options),

            // SQLite's FTS5 MATCH operator (`table MATCH 'query'`) has no
            // direct PostgreSQL equivalent. Reject it so callers know to
            // rewrite using `to_tsvector(col) @@ to_tsquery(query)` instead.
            Expr::BinaryOp { op: BinaryOperator::Match, left, .. } => {
                Err(Error::reverse_refusal(format!(
                    "SQLite FTS5 MATCH expression against {left} has no PostgreSQL operator. \
                     Rewrite using to_tsvector(col) @@ to_tsquery(query) instead."
                )))
            }

            // GLOB is case-sensitive globbing. Convert literal patterns to LIKE.
            // A character class (e.g. [abc]) or a non-literal pattern is rejected.
            Expr::BinaryOp { op: BinaryOperator::Glob, left, right } => {
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
                    Err(Error::reverse_refusal(format!(
                        "RLIKE is not SQLite syntax, so {expr} {not}RLIKE {pattern} cannot have \
                         come from SQLite. Write REGEXP instead."
                    )))
                }
            }

            // SQLite identifier `rowid` does not exist in PostgreSQL.
            Expr::Identifier(ident) if ident.value.eq_ignore_ascii_case("rowid") => {
                Err(Error::reverse_refusal(
                    "rowid: SQLite's implicit rowid column does not exist in PostgreSQL; \
                 use an explicit INTEGER PRIMARY KEY column instead"
                        .to_string(),
                ))
            }

            // `t.rowid`, `schema.table.rowid`, etc. The last segment names the
            // column; when it is `rowid` the reference is equally invalid in
            // PostgreSQL as the bare form above.
            Expr::CompoundIdentifier(parts)
                if parts.last().is_some_and(|p| p.value.eq_ignore_ascii_case("rowid")) =>
            {
                Err(Error::reverse_refusal(
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
            // A backslash escape counts as absent here, and comes back off:
            // it is what PostgreSQL's LIKE escapes with when nothing is
            // written, so the two spellings read the same, and it is the one
            // the forward direction now attaches to every emitted LIKE.
            //
            // Any other escape blocks the restore. `lower()` folds a letter
            // escape character, and the two readings then disagree: with
            // ESCAPE 'X', `'aXbc' ILIKE 'aXb_'` is false while the lowered
            // form is true.
            Expr::Like { negated, any, expr, pattern, escape_char }
                if escape_char.as_ref().is_none_or(is_backslash_escape) =>
            {
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
                    _ => translate_expr_recursive::<Reverse>(self, schema, options, &mut |_| {}),
                }
            }
            // `COLLATE NOCASE`, `COLLATE BINARY`, and `COLLATE RTRIM` are
            // SQLite-only collations with no PostgreSQL equivalent. Refuse them so
            // the reverse output is not silently rejected by the server. Unknown
            // collation names pass through because PostgreSQL allows user-defined
            // collations.
            Expr::Collate { collation, .. } => {
                let name = collation
                    .0
                    .last()
                    .and_then(ObjectNamePart::as_ident)
                    .map_or_else(|| collation.to_string(), |id| id.value.clone());
                if name.eq_ignore_ascii_case("NOCASE")
                    || name.eq_ignore_ascii_case("BINARY")
                    || name.eq_ignore_ascii_case("RTRIM")
                {
                    return Err(Error::reverse_refusal(format!(
                        "COLLATE {name} is a SQLite-only collation with no PostgreSQL \
                         equivalent. Map it to a collation registered in the destination \
                         database, or drop the COLLATE clause if byte-order ordering is \
                         acceptable."
                    )));
                }
                translate_expr_recursive::<Reverse>(self, schema, options, &mut |_| {})
            }

            _ => translate_expr_recursive::<Reverse>(self, schema, options, &mut |_| {}),
        }
    }
}

/// Builds PostgreSQL's POSIX regex match, `~` or `!~`.
fn posix_regex(
    expr: &Expr,
    pattern: &Expr,
    negated: bool,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    Ok(Expr::BinaryOp {
        left: Box::new(Expr::reverse_translate(expr, schema, options)?),
        op: if negated { BinaryOperator::PGRegexNotMatch } else { BinaryOperator::PGRegexMatch },
        right: Box::new(Expr::reverse_translate(pattern, schema, options)?),
    })
}
