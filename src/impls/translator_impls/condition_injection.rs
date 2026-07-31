//! Shared helpers for injecting IF-conditions into mutable DML statements.

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

use sqlparser::ast::{BinaryOperator, Expr, Statement};

use crate::errors::Error;

/// Combine an existing WHERE clause with an injected guard.
///
/// The existing clause is parenthesised because AND binds tighter than OR, so a
/// bare `a = 1 OR b = 2 AND guard` reassociates to `a = 1 OR (b = 2 AND guard)`
/// and the guard never reaches the first disjunct.
fn guarded(existing: Option<Expr>, condition: Expr) -> Expr {
    let Some(existing) = existing else { return condition };
    Expr::BinaryOp {
        left: Box::new(Expr::Nested(Box::new(existing))),
        op: BinaryOperator::And,
        right: Box::new(condition),
    }
}

/// Injects an `AND <condition>` predicate into mutable DML statements.
///
/// Supports:
/// - `INSERT ... SELECT ...` (injects into SELECT `WHERE`)
/// - `UPDATE` (injects/extends `WHERE`)
/// - `DELETE` (injects/extends `WHERE`)
///
/// Returns an error for statement kinds where condition injection is not
/// supported (e.g. `INSERT ... VALUES`).
pub(crate) fn inject_condition_into_dml_statement(
    stmt: &mut Statement,
    condition: Expr,
) -> Result<(), Error> {
    match stmt {
        Statement::Insert(insert) => {
            let Some(source) = &mut insert.source else {
                return Err(Error::UnsupportedSQLiteFeature(
                    "Cannot inject IF condition into INSERT without source".to_string(),
                ));
            };
            let sqlparser::ast::SetExpr::Select(select) = &mut *source.body else {
                return Err(Error::UnsupportedSQLiteFeature(
                    "Cannot inject IF condition into INSERT with non-SELECT source".to_string(),
                ));
            };
            select.selection = Some(guarded(select.selection.take(), condition));
        }
        Statement::Update(update) => {
            update.selection = Some(guarded(update.selection.take(), condition));
        }
        Statement::Delete(delete) => {
            delete.selection = Some(guarded(delete.selection.take(), condition));
        }
        _ => {
            return Err(Error::UnsupportedSQLiteFeature(
                "Cannot inject IF condition into this statement variant".to_string(),
            ));
        }
    }
    Ok(())
}
