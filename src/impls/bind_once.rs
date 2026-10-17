//! Lowerings that name an operand more than once, reading each operand once
//! where SQLite allows it.

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, format, vec::Vec};
use core::ops::ControlFlow;

use sqlparser::ast::{
    BinaryOperator, Expr, Ident, Query, SelectItem, SetExpr, TableFactor, visit_expressions,
};

use crate::{
    errors::Error,
    impls::{
        query_builder::{from_relation, make_query, make_simple_select, single_expr_query},
        replay::{is_replayable, reject_duplicated_operand},
        translator_impls::query::is_aggregate_call,
    },
    options::TranslationContext,
};

/// Builds `form` over the translated operands, keeping the output linear in the
/// source.
///
/// Atomic operands repeat as written, and an operand that does not replay is
/// refused, since a bound copy of an uncorrelated operand runs once per
/// statement instead of once per row. The rest bind once in a scalar subquery
/// where SQLite allows one and no operand holds an aggregate or window call,
/// which the subquery would evaluate over its own single row. Elsewhere they
/// repeat when they hold no other repeating lowering.
pub(crate) fn bind_once<const N: usize>(
    construct: &str,
    sources: [&Expr; N],
    translated: [Expr; N],
    options: &TranslationContext<'_>,
    form: impl Fn(&[Expr; N]) -> Expr,
) -> Result<Expr, Error> {
    let compound = || sources.iter().zip(&translated).filter(|(_, operand)| !is_atomic(operand));
    if let Some((source, _)) = compound().find(|(source, _)| !is_replayable(source, options)) {
        return Err(reject_duplicated_operand(construct, source));
    }
    if compound().next().is_none() {
        return Ok(form(&translated));
    }
    let groups = sources.iter().any(|source| holds(source, Held::AggregateOrWindow));
    if options.allows_subqueries() && !groups {
        return Ok(bound(translated, form));
    }
    if let Some((source, _)) = compound().find(|(source, _)| holds(source, Held::RepeatingLowering))
    {
        return Err(Error::forward_refusal(format!(
            "{construct} over {source} would repeat an operand that already repeats its own \
             operands, which grows the SQLite expression exponentially in this position. Compute \
             the inner value into a column first, or move the expression where SQLite allows a \
             subquery."
        )));
    }
    Ok(form(&translated))
}

fn is_atomic(expr: &Expr) -> bool {
    matches!(expr, Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Value(_))
}

fn bound<const N: usize>(translated: [Expr; N], form: impl Fn(&[Expr; N]) -> Expr) -> Expr {
    let names: [Ident; N] =
        core::array::from_fn(|index| Ident::new(format!("pg2sqlite_operand_{index}")));
    let references = names.clone().map(Expr::Identifier);
    let bindings = translated
        .into_iter()
        .zip(names)
        .map(|(expr, alias)| SelectItem::ExprWithAlias { expr, alias })
        .collect::<Vec<_>>();
    let operands: Query =
        make_query(None, SetExpr::Select(Box::new(make_simple_select(bindings, Vec::new(), None))));
    Expr::Subquery(Box::new(single_expr_query(
        form(&references),
        from_relation(TableFactor::Derived {
            lateral: false,
            subquery: Box::new(operands),
            alias: None,
            sample: None,
        }),
        None,
    )))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Held {
    AggregateOrWindow,
    RepeatingLowering,
}

/// True when `expr` holds the construct, given an operand that holds no nested
/// query.
fn holds(expr: &Expr, held: Held) -> bool {
    visit_expressions(expr, |node| {
        let found = match (held, node) {
            (Held::AggregateOrWindow, Expr::Function(call)) => {
                call.over.is_some() || is_aggregate_call(call)
            }
            (Held::RepeatingLowering, Expr::BinaryOp { op, .. }) => {
                *op == BinaryOperator::PGBitwiseXor
            }
            _ => false,
        };
        if found { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    })
    .is_break()
}
