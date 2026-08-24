//! SQLite idioms this crate invents: builder and recognizer side by side.
//!
//! The forward direction lowers a PostgreSQL construct onto a SQLite shape no
//! grammar owns, and the reverse direction must read that exact shape back to
//! translate the crate's own output. Each pair lives here with a round-trip
//! test, so a forward idiom cannot change or appear without its way home
//! (review findings H6 and M6 were exactly that drift).

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{boxed::Box, string::ToString, vec, vec::Vec};

use sqlparser::ast::{
    BinaryOperator, CastKind, DataType, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArguments, Value, ValueWithSpan,
};

use crate::impls::{
    expr_helpers::case_when,
    function_helpers::{integer_literal, number_literal, simple_function_expr, string_literal},
};

// ── ILIKE case folding ──────────────────────────────────────────────────────

/// `lower(<expr>)`, the case fold ILIKE lowers onto.
#[must_use]
pub(crate) fn wrap_with_lower(expr: Expr) -> Expr {
    simple_function_expr("lower", vec![expr], None)
}

/// The argument of a `lower(...)` call the forward ILIKE rewrite emitted.
///
/// Rebuilding the call through the builder and comparing is what keeps the two
/// directions in step: a `lower` carrying a window, a filter, a modifier, or a
/// different spelling is not what the rewrite emits, so it is not restored.
#[must_use]
pub(crate) fn forward_lower_argument(expr: &Expr) -> Option<&Expr> {
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

// ── random() over the unit interval ─────────────────────────────────────────

/// PostgreSQL's `random()` (a double in `[0, 1)`) as
/// `(CAST(random() AS REAL) + 9223372036854775808.0) / 18446744073709551616.0`.
///
/// The shift-then-divide form avoids `ABS(-9223372036854775808)` overflow in
/// SQLite, whose `random()` answers a signed 64-bit integer.
#[must_use]
pub(crate) fn uniform_random_float() -> Expr {
    let random_call = simple_function_expr("random", vec![], None);
    let random_as_real = Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(random_call),
        data_type: DataType::Real,
        format: None,
    };
    let shifted = Expr::BinaryOp {
        left: Box::new(random_as_real),
        op: BinaryOperator::Plus,
        right: Box::new(number_literal("9223372036854775808.0")),
    };
    Expr::BinaryOp {
        left: Box::new(Expr::Nested(Box::new(shifted))),
        op: BinaryOperator::Divide,
        right: Box::new(number_literal("18446744073709551616.0")),
    }
}

/// True when `expr` is exactly what [`uniform_random_float`] builds.
#[must_use]
pub(crate) fn is_uniform_random_float(expr: &Expr) -> bool {
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

// ── LOCALTIMESTAMP and LOCALTIME ────────────────────────────────────────────

/// The argument list `('now', 'localtime')`, which `datetime(...)` and
/// `time(...)` take to answer PostgreSQL's `localtimestamp` and `localtime`.
#[must_use]
pub(crate) fn now_localtime_args() -> Vec<Expr> {
    vec![string_literal("now"), string_literal("localtime")]
}

/// True when `args` is exactly what [`now_localtime_args`] builds.
#[must_use]
pub(crate) fn is_now_localtime_args(args: &FunctionArguments) -> bool {
    let FunctionArguments::List(list) = args else { return false };
    let [first, second] = list.args.as_slice() else { return false };
    let is_now = matches!(
        first,
        FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(ValueWithSpan {
            value: Value::SingleQuotedString(s), ..
        }))) if s == "now"
    );
    let is_localtime = matches!(
        second,
        FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(ValueWithSpan {
            value: Value::SingleQuotedString(s), ..
        }))) if s == "localtime"
    );
    is_now && is_localtime
}

// ── ascii over a possibly empty string ──────────────────────────────────────

/// PostgreSQL's `ascii(x)` as `CASE WHEN x = '' THEN 0 ELSE unicode(x) END`.
///
/// The two functions agree on every input except the empty string, where
/// PostgreSQL answers 0 and SQLite's `unicode` answers NULL, measured on 18
/// and 3.51. A NULL operand falls to the ELSE branch (`NULL = ''` is NULL)
/// and stays NULL through `unicode`. `x` is read twice, which is only
/// observable for a volatile operand.
#[must_use]
pub(crate) fn ascii_code_point(expr: Expr) -> Expr {
    case_when(
        Expr::BinaryOp {
            left: Box::new(expr.clone()),
            op: BinaryOperator::Eq,
            right: Box::new(string_literal("")),
        },
        integer_literal(0),
        Some(simple_function_expr("unicode", vec![expr], None)),
    )
}

/// The argument of the [`ascii_code_point`] shape, or `None` for anything
/// else. Rebuild-and-compare, like [`forward_lower_argument`].
#[must_use]
pub(crate) fn ascii_code_point_argument(expr: &Expr) -> Option<&Expr> {
    let Expr::Case { else_result: Some(else_result), .. } = expr else {
        return None;
    };
    let Expr::Function(unicode_call) = else_result.as_ref() else {
        return None;
    };
    let FunctionArguments::List(list) = &unicode_call.args else {
        return None;
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(argument))] = list.args.as_slice() else {
        return None;
    };
    (ascii_code_point(argument.clone()) == *expr).then_some(argument)
}

// ── empty-set aggregates ─────────────────────────────────────────────────────

/// `NULLIF(<aggregate>, '[]')`: `json_group_array` answers `'[]'` over no
/// rows where PostgreSQL's `json_agg` and `array_agg` answer NULL, and a
/// non-empty aggregate always carries an element, so `'[]'` can only mean no
/// rows.
#[must_use]
pub(crate) fn nullif_empty_json_array(aggregate: Expr) -> Expr {
    simple_function_expr("NULLIF", vec![aggregate, string_literal("[]")], None)
}

/// `NULLIF(<aggregate>, '{}')`: the [`nullif_empty_json_array`] treatment for
/// `json_group_object`, whose empty answer is `'{}'`.
#[must_use]
pub(crate) fn nullif_empty_json_object(aggregate: Expr) -> Expr {
    simple_function_expr("NULLIF", vec![aggregate, string_literal("{}")], None)
}

/// Returns the inner `Function` if `func` matches
/// `NULLIF(json_group_array(...), '[]')`, the shape
/// [`nullif_empty_json_array`] builds for `json_agg` and `array_agg`.
#[must_use]
pub(crate) fn extract_json_group_array_nullif(func: &Function) -> Option<&Function> {
    extract_nullif_wrapped(func, "json_group_array", "[]")
}

/// Returns the inner `Function` if `func` matches
/// `NULLIF(json_group_object(...), '{}')`, the shape
/// [`nullif_empty_json_object`] builds for `json_object_agg`.
#[must_use]
pub(crate) fn extract_json_group_object_nullif(func: &Function) -> Option<&Function> {
    extract_nullif_wrapped(func, "json_group_object", "{}")
}

/// The shared shape of the two extractors: `NULLIF(<inner_name>(...),
/// '<sentinel>')` with the inner call as the first argument.
fn extract_nullif_wrapped<'e>(
    func: &'e Function,
    inner_name: &str,
    sentinel: &str,
) -> Option<&'e Function> {
    let outer_name = func.name.0.last().and_then(|p| p.as_ident())?;
    if !outer_name.value.eq_ignore_ascii_case("nullif") {
        return None;
    }
    let FunctionArguments::List(list) = &func.args else { return None };
    let [first, second] = list.args.as_slice() else { return None };
    let FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Function(inner))) = first else {
        return None;
    };
    let found = inner.name.0.last().and_then(|p| p.as_ident())?;
    if !found.value.eq_ignore_ascii_case(inner_name) {
        return None;
    }
    let FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(ValueWithSpan {
        value: Value::SingleQuotedString(found_sentinel),
        ..
    }))) = second
    else {
        return None;
    };
    (found_sentinel == sentinel).then_some(inner)
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sqlparser::ast::{Function, Ident};

    use super::*;

    fn ident_expr(name: &str) -> Expr {
        Expr::Identifier(Ident::new(name))
    }

    fn call(name: &str, args: Vec<Expr>) -> Function {
        call_of(simple_function_expr(name, args, None))
    }

    #[test]
    fn the_lower_wrap_round_trips() {
        let wrapped = wrap_with_lower(ident_expr("name"));
        assert_eq!(forward_lower_argument(&wrapped), Some(&ident_expr("name")));
        // A different spelling is not what the rewrite emits.
        assert_eq!(forward_lower_argument(&ident_expr("lower")), None);
    }

    #[test]
    fn the_uniform_random_shape_round_trips() {
        assert!(is_uniform_random_float(&uniform_random_float()));
        assert!(!is_uniform_random_float(&ident_expr("random")));
    }

    #[test]
    fn the_now_localtime_arguments_round_trip() {
        let function = call("datetime", now_localtime_args());
        assert!(is_now_localtime_args(&function.args));
        let reversed = call("datetime", vec![string_literal("localtime"), string_literal("now")]);
        assert!(!is_now_localtime_args(&reversed.args), "argument order is part of the shape");
    }

    #[test]
    fn the_empty_array_nullif_round_trips() {
        let aggregate = simple_function_expr("json_group_array", vec![ident_expr("v")], None);
        let wrapped = call_of(nullif_empty_json_array(aggregate));
        let inner = extract_json_group_array_nullif(&wrapped).expect("the shape must round-trip");
        assert!(inner.name.to_string().eq_ignore_ascii_case("json_group_array"));
        // The object sentinel does not claim the array shape.
        assert!(extract_json_group_object_nullif(&wrapped).is_none());
    }

    #[test]
    fn the_empty_object_nullif_round_trips() {
        let aggregate =
            simple_function_expr("json_group_object", vec![ident_expr("k"), ident_expr("v")], None);
        let wrapped = call_of(nullif_empty_json_object(aggregate));
        let inner = extract_json_group_object_nullif(&wrapped).expect("the shape must round-trip");
        assert!(inner.name.to_string().eq_ignore_ascii_case("json_group_object"));
        assert!(extract_json_group_array_nullif(&wrapped).is_none());
    }

    /// A mismatched sentinel is not the idiom: `NULLIF(json_group_array(x),
    /// '{}')` extracts as neither shape.
    #[test]
    fn a_crossed_sentinel_is_not_extracted() {
        let aggregate = simple_function_expr("json_group_array", vec![ident_expr("v")], None);
        let crossed = call_of(nullif_empty_json_object(aggregate));
        assert!(extract_json_group_array_nullif(&crossed).is_none());
        assert!(extract_json_group_object_nullif(&crossed).is_none());
    }

    fn call_of(expr: Expr) -> Function {
        let Expr::Function(function) = expr else {
            unreachable!("the NULLIF builders build a Function")
        };
        function
    }
    #[test]
    fn the_ascii_shape_round_trips() {
        let built = ascii_code_point(ident_expr("s"));
        assert_eq!(ascii_code_point_argument(&built), Some(&ident_expr("s")));
        assert_eq!(ascii_code_point_argument(&ident_expr("s")), None);
    }

    /// A CASE whose two argument reads differ is not the idiom.
    #[test]
    fn a_case_with_mismatched_arguments_is_not_extracted() {
        let mut crossed = ascii_code_point(ident_expr("s"));
        let Expr::Case { else_result: Some(else_result), .. } = &mut crossed else {
            unreachable!("the builder builds a CASE with an ELSE")
        };
        **else_result = simple_function_expr("unicode", vec![ident_expr("other")], None);
        assert_eq!(ascii_code_point_argument(&crossed), None);
    }

    /// The recognizer must also claim the shape after a print-and-parse round
    /// trip, since the reverse direction reads parsed text, not the built
    /// tree.
    #[test]
    fn the_ascii_shape_survives_a_parse() {
        let printed = format!("SELECT {}", ascii_code_point(ident_expr("s")));
        let statements =
            sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::SQLiteDialect {}, &printed)
                .expect("the emitted shape parses as SQLite");
        let sqlparser::ast::Statement::Query(query) = &statements[0] else {
            unreachable!("a SELECT parses as a query")
        };
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
            unreachable!("a plain SELECT has a select body")
        };
        let sqlparser::ast::SelectItem::UnnamedExpr(parsed) = &select.projection[0] else {
            unreachable!("the projection is a bare expression")
        };
        assert_eq!(ascii_code_point_argument(parsed), Some(&ident_expr("s")));
    }
}
