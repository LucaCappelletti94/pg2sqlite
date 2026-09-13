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
        BinaryOperator, CaseWhen, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Ident,
        ObjectName, ObjectNamePart, UnaryOperator, Value, ValueWithSpan,
        helpers::attached_token::AttachedToken,
    },
    tokenizer::Span,
};

use super::{
    function::reverse_translate_function,
    helpers::{Reverse, unscale_integer_literal},
};
use crate::{
    errors::Error,
    impls::{
        function_helpers::{simple_function_expr, single_quoted_literal, string_literal},
        idioms::{ascii_code_point_argument, forward_lower_argument, is_uniform_random_float},
        reverse_translator_impls::ident_quoting::is_postgres_pseudo_expression,
        shared_helpers::{
            declared_in_scope, declared_type_matches, is_scale_preserving_call, scale_of,
            translate_expr_recursive,
        },
        temporal_arithmetic::reverse_temporal_arithmetic,
        translator_impls::expr::sqlite_json_path_to_pg_text_path,
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
        Some(Box::new(Expr::Value(ValueWithSpan {
            value: Value::SingleQuotedString("\\".to_string()),
            span: Span::empty(),
        })))
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
fn is_backslash_escape(escape: &Expr) -> bool {
    matches!(
        escape,
        Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(character), .. })
            if character == "\\"
    )
}

/// True when `op` and its operands could involve a NUMERIC column's minor
/// units, so numeric scale lookup is worthwhile.
fn reverse_numeric_rules_may_apply(op: &BinaryOperator, left: &Expr, right: &Expr) -> bool {
    if !matches!(
        op,
        BinaryOperator::Plus
            | BinaryOperator::Minus
            | BinaryOperator::Multiply
            | BinaryOperator::Divide
            | BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq
    ) {
        return false;
    }
    fn addressable(expr: &Expr) -> bool {
        match expr {
            Expr::Identifier(_)
            | Expr::CompoundIdentifier(_)
            | Expr::Nested(_)
            | Expr::Cast { .. } => true,
            Expr::UnaryOp { op: UnaryOperator::Minus | UnaryOperator::Plus, expr } => {
                addressable(expr)
            }
            Expr::Function(f) => is_scale_preserving_call(f),
            _ => false,
        }
    }
    fn numeric_candidate(expr: &Expr) -> bool {
        match expr {
            Expr::Value(ValueWithSpan { value: Value::Number(_, _), .. }) => true,
            Expr::Value(_) => false,
            Expr::Nested(inner) => numeric_candidate(inner),
            Expr::UnaryOp { op: UnaryOperator::Minus | UnaryOperator::Plus, expr } => {
                numeric_candidate(expr)
            }
            other => addressable(other),
        }
    }
    (addressable(left) || addressable(right)) && numeric_candidate(left) && numeric_candidate(right)
}

/// Unscale `expr` if it is an integer literal at `scale`, otherwise
/// reverse-translate it normally.
fn reverse_unscale_or_translate(
    expr: &Expr,
    scale: Option<u32>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    if let Some(scale) = scale.filter(|&s| s > 0)
        && let Some(unscaled) = unscale_integer_literal(expr, scale)
    {
        return Ok(unscaled);
    }
    expr.reverse_translate(schema, options)
}

/// Rescales integer literals inside an already-translated scale-preserving
/// call.
fn reverse_scale_call_arguments(call: Expr, scale: u32) -> Result<Expr, Error> {
    let Expr::Function(mut function) = call else { return Ok(call) };
    if !is_scale_preserving_call(&function) {
        return Ok(Expr::Function(function));
    }
    if let FunctionArguments::List(list) = &mut function.args {
        for argument in &mut list.args {
            let (FunctionArg::Named { arg, .. }
            | FunctionArg::ExprNamed { arg, .. }
            | FunctionArg::Unnamed(arg)) = argument;
            let FunctionArgExpr::Expr(expr) = arg else { continue };
            if let Some(unscaled) = unscale_integer_literal(expr, scale) {
                *expr = unscaled;
                continue;
            }
            if matches!(expr, Expr::Function(_)) {
                let taken = core::mem::replace(expr, Expr::Wildcard(AttachedToken::empty()));
                *expr = reverse_scale_call_arguments(taken, scale)?;
            }
        }
    }
    Ok(Expr::Function(function))
}

/// True when `expr` is definitely non-text, so `||` would fail in PostgreSQL.
fn is_definitely_non_text(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> bool {
    match expr {
        Expr::Value(ValueWithSpan { value: Value::Number(_, _) | Value::Boolean(_), .. }) => true,
        Expr::UnaryOp { op: UnaryOperator::Minus | UnaryOperator::Plus, expr: inner } => {
            is_definitely_non_text(inner, schema, options)
        }
        _ => {
            declared_type_matches(expr, schema, options, |declared| {
                let lower = declared.to_ascii_lowercase();
                lower.starts_with("int")
                    || lower.starts_with("real")
                    || lower.starts_with("float")
                    || lower.starts_with("double")
                    || lower.starts_with("numeric")
                    || lower.starts_with("decimal")
                    || lower.starts_with("bool")
                    || lower == "bigint"
                    || lower == "smallint"
                    || lower == "serial"
                    || lower == "bigserial"
            })
            .unwrap_or(false)
        }
    }
}

/// A brief description of what a PostgreSQL pseudo-expression returns, for use
/// in error messages.
fn pseudo_expression_kind(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "user" | "session_user" | "current_user" => "the current role",
        "current_schema" => "the current schema name",
        "current_date" => "today's date",
        "current_time" | "localtime" => "the current time",
        "current_timestamp" | "localtimestamp" => "the current timestamp",
        _ => "a server-side value",
    }
}

/// Quotes a column whose name PostgreSQL reads as an expression.
///
/// `SELECT user FROM t` answers the stored value on the replica and the
/// current role at the server, so a name the schema confirms is a column is
/// emitted quoted. A name the schema cannot confirm is refused rather than
/// guessed at, since quoting an actual pseudo-expression would turn a
/// server-side value into a missing column.
fn reverse_pseudo_expression_name(
    whole: &Expr,
    ident: &Ident,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    if declared_in_scope(whole, schema, options, |_| Some(()), |_, _, _| Ok(Some(())))
        .is_ok_and(|declared| declared.is_some())
    {
        return Ok(Expr::Identifier(Ident::with_quote('"', &ident.value)));
    }
    Err(Error::reverse_refusal(format!(
        "bare '{}' is a PostgreSQL pseudo-expression returning {}; quote it as \"{}\" to \
         reference a column, or include the table in the translation batch so the schema can \
         confirm it is a column.",
        ident.value,
        pseudo_expression_kind(&ident.value),
        ident.value,
    )))
}

impl ReverseTranslator for Expr {
    type Schema = ParserDB;
    type PostgresEntry = Self;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<Self::PostgresEntry, Error> {
        if let Some(restored) = restore_lowered_idiom(self, schema, options) {
            return restored;
        }

        match self {
            Expr::Function(func) => reverse_numeric_call(self, func, schema, options),

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

            Expr::Identifier(ident)
                if ident.quote_style.is_none() && is_postgres_pseudo_expression(&ident.value) =>
            {
                reverse_pseudo_expression_name(self, ident, schema, options)
            }

            Expr::Like { negated, any, expr, pattern, escape_char }
                if escape_char.as_ref().is_none_or(|escape| is_backslash_escape(escape)) =>
            {
                reverse_like(self, *negated, *any, expr, pattern, schema, options)
            }
            Expr::Collate { collation, .. } => reverse_collate(self, collation, schema, options),

            // SQLite uses JSONPath ('$.a'), PostgreSQL uses text-array ('{a}'); convert.
            Expr::BinaryOp {
                op: op @ (BinaryOperator::HashArrow | BinaryOperator::HashLongArrow),
                left,
                right,
            } => reverse_json_path_operator(op, left, right, schema, options),

            Expr::BinaryOp { op: BinaryOperator::StringConcat, left, right } => {
                reverse_string_concat(self, left, right, schema, options)
            }

            Expr::BinaryOp { left, op, right } => {
                reverse_numeric_binary_op(self, left, op, right, schema, options)
            }

            Expr::InList { expr: operand, list, negated } => {
                let scale = scale_of(operand, schema, options).filter(|&scale| scale > 0);
                Ok(Expr::InList {
                    expr: Box::new(operand.reverse_translate(schema, options)?),
                    list: list
                        .iter()
                        .map(|item| reverse_unscale_or_translate(item, scale, schema, options))
                        .collect::<Result<Vec<_>, _>>()?,
                    negated: *negated,
                })
            }

            Expr::Between { expr: operand, negated, low, high } => {
                let scale = scale_of(operand, schema, options).filter(|&scale| scale > 0);
                Ok(Expr::Between {
                    expr: Box::new(operand.reverse_translate(schema, options)?),
                    negated: *negated,
                    low: Box::new(reverse_unscale_or_translate(low, scale, schema, options)?),
                    high: Box::new(reverse_unscale_or_translate(high, scale, schema, options)?),
                })
            }

            Expr::Case { case_token, end_token, operand, conditions, else_result } => {
                reverse_numeric_case(
                    case_token,
                    end_token,
                    operand.as_deref(),
                    conditions,
                    else_result.as_deref(),
                    schema,
                    options,
                )
            }

            _ => translate_expr_recursive::<Reverse>(self, schema, options, &mut |_| {}),
        }
    }
}

/// Restores `lower(x) LIKE lower(y)` to `x ILIKE y`, and leaves a plain
/// `LIKE` alone.
///
/// Measured on PostgreSQL 16, the two readings agree, non-ASCII case and
/// wildcard patterns included. A plain `LIKE` needs no rewrite because the
/// forward direction emits `PRAGMA case_sensitive_like = true`, so the
/// replica reads `LIKE` the case-sensitive way PostgreSQL does.
fn reverse_like(
    whole: &Expr,
    negated: bool,
    any: bool,
    expr: &Expr,
    pattern: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    match (forward_lower_argument(expr), forward_lower_argument(pattern)) {
        (Some(subject), Some(target)) => {
            Ok(Expr::ILike {
                negated,
                any,
                expr: Box::new(subject.reverse_translate(schema, options)?),
                pattern: Box::new(target.reverse_translate(schema, options)?),
                escape_char: None,
            })
        }
        _ => translate_expr_recursive::<Reverse>(whole, schema, options, &mut |_| {}),
    }
}

/// Reverses a `COLLATE`, refusing the collations only SQLite has.
///
/// `NOCASE`, `BINARY` and `RTRIM` name no PostgreSQL collation, so the
/// server would reject the statement. Any other name passes through, since
/// PostgreSQL takes user-defined collations.
fn reverse_collate(
    whole: &Expr,
    collation: &ObjectName,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    let name = collation
        .0
        .last()
        .and_then(ObjectNamePart::as_ident)
        .map_or_else(|| collation.to_string(), |part| part.value.clone());
    if ["NOCASE", "BINARY", "RTRIM"].iter().any(|only| name.eq_ignore_ascii_case(only)) {
        return Err(Error::reverse_refusal(format!(
            "COLLATE {name} is a SQLite-only collation with no PostgreSQL equivalent. Map it to \
             a collation registered in the destination database, or drop the COLLATE clause if \
             byte-order ordering is acceptable."
        )));
    }
    translate_expr_recursive::<Reverse>(whole, schema, options, &mut |_| {})
}

/// Reverses `||`, refusing an operand PostgreSQL will not concatenate.
///
/// SQLite converts any operand to text, and PostgreSQL has `text || text`
/// and no `text || integer`, so an operand the schema shows is numeric or
/// boolean would fail at the server.
fn reverse_string_concat(
    whole: &Expr,
    left: &Expr,
    right: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    for (side, operand) in [("left", left), ("right", right)] {
        if is_definitely_non_text(operand, schema, options) {
            return Err(Error::reverse_refusal(format!(
                "|| {side} operand ({operand}) is not text; PostgreSQL's || requires text on \
                 both sides. Cast it first: ({operand})::text"
            )));
        }
    }
    translate_expr_recursive::<Reverse>(whole, schema, options, &mut |_| {})
}

/// Reverses a call, bringing the literal arguments of a scale-preserving one
/// back from minor units: `coalesce(amount, 150)` reads `coalesce(amount,
/// 1.50)` at the server.
fn reverse_numeric_call(
    whole: &Expr,
    func: &sqlparser::ast::Function,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    let translated = reverse_translate_function(func, schema, options)?;
    match scale_of(whole, schema, options) {
        Some(scale) if scale > 0 => reverse_scale_call_arguments(translated, scale),
        _ => Ok(translated),
    }
}

/// Restores a shape the forward direction lowered, before the ordinary walk
/// can take it apart.
///
/// Each of these lowerings spells a PostgreSQL function with SQLite pieces
/// that carry no PostgreSQL name of their own, so reversing the pieces one at
/// a time would refuse the statement or, for `ascii`, answer NULL where the
/// original answered zero.
fn restore_lowered_idiom(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Option<Result<Expr, Error>> {
    if is_uniform_random_float(expr) {
        return Some(Ok(simple_function_expr("random", vec![], None)));
    }
    if let Some(argument) = ascii_code_point_argument(expr) {
        return Some(
            argument
                .reverse_translate(schema, options)
                .map(|argument| simple_function_expr("ascii", vec![argument], None)),
        );
    }
    reverse_temporal_arithmetic(expr, schema, options)
}

/// Brings the literal arms of a `CASE` back to the scale its other arms
/// carry, and the `WHEN` literals of the simple form back to the operand's.
fn reverse_numeric_case(
    case_token: &AttachedToken,
    end_token: &AttachedToken,
    operand: Option<&Expr>,
    conditions: &[CaseWhen],
    else_result: Option<&Expr>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    let result_scale = conditions
        .iter()
        .map(|arm| &arm.result)
        .chain(else_result)
        .filter_map(|result| scale_of(result, schema, options))
        .filter(|&scale| scale > 0)
        .max();
    let operand_scale =
        operand.and_then(|operand| scale_of(operand, schema, options)).filter(|&scale| scale > 0);

    Ok(Expr::Case {
        case_token: case_token.clone(),
        end_token: end_token.clone(),
        operand: operand
            .map(|operand| operand.reverse_translate(schema, options))
            .transpose()?
            .map(Box::new),
        conditions: conditions
            .iter()
            .map(|arm| {
                Ok(CaseWhen {
                    condition: reverse_unscale_or_translate(
                        &arm.condition,
                        operand_scale,
                        schema,
                        options,
                    )?,
                    result: reverse_unscale_or_translate(
                        &arm.result,
                        result_scale,
                        schema,
                        options,
                    )?,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?,
        else_result: else_result
            .map(|result| reverse_unscale_or_translate(result, result_scale, schema, options))
            .transpose()?
            .map(Box::new),
    })
}

/// Brings an integer literal back to the decimal PostgreSQL means when the
/// other side of the operation is a `NUMERIC` column held as minor units.
///
/// Division is refused instead: the replica already truncated its integer
/// division over minor units, and the server would answer the exact numeric
/// quotient, so no literal adjustment makes the two agree.
fn reverse_numeric_binary_op(
    whole: &Expr,
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    if !reverse_numeric_rules_may_apply(op, left, right) {
        return translate_expr_recursive::<Reverse>(whole, schema, options, &mut |_| {});
    }

    let left_scale = scale_of(left, schema, options);
    let right_scale = scale_of(right, schema, options);
    if *op == BinaryOperator::Divide
        && (left_scale.is_some_and(|scale| scale > 0) || right_scale.is_some_and(|scale| scale > 0))
    {
        return Err(Error::reverse_refusal(format!(
            "division involving a NUMERIC column cannot be faithfully reversed ({left} / \
             {right}): the replica performed integer division over minor units and PostgreSQL \
             will give exact numeric division. Rewrite the query to avoid dividing a NUMERIC \
             column on the replica."
        )));
    }

    if let Some(scale) = left_scale.filter(|&scale| scale > 0)
        && let Some(unscaled) = unscale_integer_literal(right, scale)
    {
        return Ok(Expr::BinaryOp {
            left: Box::new(left.reverse_translate(schema, options)?),
            op: op.clone(),
            right: Box::new(unscaled),
        });
    }
    if let Some(scale) = right_scale.filter(|&scale| scale > 0)
        && let Some(unscaled) = unscale_integer_literal(left, scale)
    {
        return Ok(Expr::BinaryOp {
            left: Box::new(unscaled),
            op: op.clone(),
            right: Box::new(right.reverse_translate(schema, options)?),
        });
    }
    translate_expr_recursive::<Reverse>(whole, schema, options, &mut |_| {})
}

/// Rewrites `#>` and `#>>` from SQLite's JSONPath right operand onto
/// PostgreSQL's `text[]` one.
///
/// The same text means different paths in the two engines: SQLite reads
/// `'$.a'` as the key `a`, and PostgreSQL reads it as a one-element array
/// holding the three characters, so the lookup finds nothing and answers
/// NULL. A path this cannot read is refused rather than passed through.
fn reverse_json_path_operator(
    op: &BinaryOperator,
    left: &Expr,
    right: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    let operator = if *op == BinaryOperator::HashLongArrow { "#>>" } else { "#>" };
    let Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(path), .. }) = right else {
        return Err(Error::reverse_refusal(format!(
            "{operator} needs its right operand as a string literal; non-literal paths cannot be \
             converted at translation time"
        )));
    };
    let pg_path = sqlite_json_path_to_pg_text_path(path).ok_or_else(|| {
        Error::reverse_refusal(format!(
            "{operator} path '{path}' cannot be converted: only simple dotted paths like '$.a' or \
             '$.a.b' are supported"
        ))
    })?;
    Ok(Expr::BinaryOp {
        left: Box::new(left.reverse_translate(schema, options)?),
        op: op.clone(),
        right: Box::new(string_literal(&pg_path)),
    })
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
