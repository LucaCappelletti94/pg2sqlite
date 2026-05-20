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
            if let Some(source) = &mut insert.source {
                match &mut *source.body {
                    sqlparser::ast::SetExpr::Select(select) => {
                        let new_selection = if let Some(existing) = &select.selection {
                            Expr::BinaryOp {
                                left: Box::new(existing.clone()),
                                op: BinaryOperator::And,
                                right: Box::new(condition),
                            }
                        } else {
                            condition
                        };
                        select.selection = Some(new_selection);
                    }
                    _ => {
                        return Err(Error::UnsupportedSQLiteFeature(
                            "Cannot inject IF condition into INSERT with non-SELECT source"
                                .to_string(),
                        ));
                    }
                }
            } else {
                return Err(Error::UnsupportedSQLiteFeature(
                    "Cannot inject IF condition into INSERT without source".to_string(),
                ));
            }
        }
        Statement::Update(update) => {
            let new_selection = if let Some(existing) = &update.selection {
                Expr::BinaryOp {
                    left: Box::new(existing.clone()),
                    op: BinaryOperator::And,
                    right: Box::new(condition),
                }
            } else {
                condition
            };
            update.selection = Some(new_selection);
        }
        Statement::Delete(delete) => {
            let new_selection = if let Some(existing) = &delete.selection {
                Expr::BinaryOp {
                    left: Box::new(existing.clone()),
                    op: BinaryOperator::And,
                    right: Box::new(condition),
                }
            } else {
                condition
            };
            delete.selection = Some(new_selection);
        }
        _ => {
            return Err(Error::UnsupportedSQLiteFeature(
                "Cannot inject IF condition into this statement variant".to_string(),
            ));
        }
    }
    Ok(())
}
