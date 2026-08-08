//! Shared AST construction helpers for building function calls, literals,
//! and argument extraction. Used by both forward and reverse function
//! translators to reduce boilerplate.

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

use sqlparser::ast::{
    Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentList, FunctionArguments, Ident,
    ObjectName, ObjectNamePart, UnaryOperator, Value, ValueWithSpan, WindowType,
};

use crate::errors::Error;

/// Create an unnamed function argument from an expression.
#[must_use]
pub(crate) fn unnamed_arg(expr: Expr) -> FunctionArg {
    FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))
}

/// Create a single-quoted string literal expression.
#[must_use]
pub(crate) fn string_literal(s: &str) -> Expr {
    Expr::Value(ValueWithSpan {
        value: Value::SingleQuotedString(s.to_string()),
        span: sqlparser::tokenizer::Span::empty(),
    })
}

/// The text of `expr` when it is a single-quoted string literal.
///
/// The read next to [`string_literal`], which is the write. Borrows rather
/// than cloning, since several callers only compare the text.
///
/// Single quotes only. A double-quoted `"x"` is an identifier in PostgreSQL
/// and parses as one under both dialects this crate uses, so
/// `Value::DoubleQuotedString` never reaches a translator and an arm for it
/// would be dead.
#[must_use]
pub(crate) fn single_quoted_literal(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(text), .. }) => Some(text),
        _ => None,
    }
}

/// Create a numeric literal expression from a string representation.
#[must_use]
pub(crate) fn number_literal(n: &str) -> Expr {
    Expr::Value(ValueWithSpan {
        value: Value::Number(n.to_string(), false),
        span: sqlparser::tokenizer::Span::empty(),
    })
}

/// Create a SQLite-compatible integer literal expression.
#[must_use]
pub(crate) fn integer_literal(value: i64) -> Expr {
    Expr::Value(ValueWithSpan {
        value: Value::Number(value.to_string(), false),
        span: sqlparser::tokenizer::Span::empty(),
    })
}

/// The value of `expr` when it is an integer literal, with an optional sign.
///
/// The read next to [`integer_literal`], which is the write. Callers that need
/// a narrower type convert at the call site, so the range a caller accepts is
/// stated where that caller decides it rather than here.
#[must_use]
pub(crate) fn integer_literal_value(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Value(ValueWithSpan { value: Value::Number(digits, _), .. }) => digits.parse().ok(),
        Expr::Nested(inner) | Expr::UnaryOp { op: UnaryOperator::Plus, expr: inner } => {
            integer_literal_value(inner)
        }
        Expr::UnaryOp { op: UnaryOperator::Minus, expr: inner } => {
            integer_literal_value(inner).map(i64::wrapping_neg)
        }
        _ => None,
    }
}

/// Build a simple function call expression with unnamed positional arguments
/// and an optional window specification.
#[must_use]
pub(crate) fn simple_function_expr(name: &str, args: Vec<Expr>, over: Option<WindowType>) -> Expr {
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: args.into_iter().map(unnamed_arg).collect(),
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over,
        within_group: vec![],
    })
}

/// Extract an expression from a function argument, returning an error for
/// non-expression arguments (wildcards, qualified wildcards).
pub(crate) fn function_arg_expr_or_err(arg: &FunctionArg) -> Result<&Expr, Error> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
        | FunctionArg::Named { arg: FunctionArgExpr::Expr(e), .. }
        | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(e), .. } => Ok(e),
        _ => {
            Err(Error::UnsupportedSQLiteFeature(
                "Expected expression argument in function".to_string(),
            ))
        }
    }
}

/// Extract exactly `count` expression references from function arguments,
/// returning an error naming `func_name` if the count doesn't match.
pub(crate) fn extract_exactly<'a>(
    args: &'a FunctionArguments,
    count: usize,
    func_name: &str,
) -> Result<Vec<&'a Expr>, Error> {
    let exprs: Vec<&Expr> = crate::impls::shared_helpers::function_argument_exprs(args);
    if exprs.len() != count {
        return Err(Error::UnsupportedSQLiteFeature(format!(
            "{func_name}() requires exactly {count} argument{}",
            if count == 1 { "" } else { "s" }
        )));
    }
    Ok(exprs)
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sqlparser::ast::{FunctionArg, FunctionArgExpr, FunctionArgumentList, FunctionArguments};

    use super::*;

    #[test]
    fn string_literal_produces_single_quoted() {
        let expr = string_literal("now");
        assert_eq!(expr.to_string(), "'now'");
    }

    #[test]
    fn number_literal_produces_number() {
        let expr = number_literal("42");
        assert_eq!(expr.to_string(), "42");
    }

    #[test]
    fn integer_literal_produces_number() {
        let expr = integer_literal(7);
        assert_eq!(expr.to_string(), "7");
    }

    #[test]
    fn simple_function_expr_builds_call() {
        let expr = simple_function_expr("datetime", vec![string_literal("now")], None);
        assert_eq!(expr.to_string(), "datetime('now')");
    }

    #[test]
    fn extract_exactly_validates_count() {
        let args = FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(string_literal("a")))],
            clauses: vec![],
        });
        assert!(extract_exactly(&args, 1, "test").is_ok());
        assert!(extract_exactly(&args, 2, "test").is_err());
    }

    #[test]
    fn function_arg_expr_or_err_rejects_wildcard() {
        let wildcard = FunctionArg::Unnamed(FunctionArgExpr::Wildcard);
        assert!(function_arg_expr_or_err(&wildcard).is_err());
    }
}
