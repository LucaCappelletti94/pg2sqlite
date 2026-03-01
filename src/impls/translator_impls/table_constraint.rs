//! Implementation of the [`Translator`] trait for the
//! `TableConstraint` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{AccessExpr, Array, Expr, Function, TableConstraint};

use crate::{
    impls::object_name::{append_suffix, table_has_implicit_public_rls},
    options::Pg2SqliteOptions,
    prelude::{TranslationOptions, Translator},
};

fn first_function_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Function(Function { name, .. }) => Some(name.to_string()),
        Expr::UnaryOp { expr, .. }
        | Expr::Cast { expr, .. }
        | Expr::Extract { expr, .. }
        | Expr::Ceil { expr, .. }
        | Expr::Floor { expr, .. }
        | Expr::Nested(expr)
        | Expr::IsNull(expr)
        | Expr::IsNotNull(expr)
        | Expr::IsUnknown(expr)
        | Expr::IsNotUnknown(expr)
        | Expr::IsTrue(expr)
        | Expr::IsNotTrue(expr)
        | Expr::IsFalse(expr)
        | Expr::IsNotFalse(expr)
        | Expr::Prefixed { value: expr, .. }
        | Expr::Collate { expr, .. } => first_function_name(expr),
        Expr::BinaryOp { left, right, .. }
        | Expr::AnyOp { left, right, .. }
        | Expr::AllOp { left, right, .. }
        | Expr::Like { expr: left, pattern: right, .. }
        | Expr::ILike { expr: left, pattern: right, .. }
        | Expr::SimilarTo { expr: left, pattern: right, .. }
        | Expr::RLike { expr: left, pattern: right, .. }
        | Expr::Position { expr: left, r#in: right }
        | Expr::AtTimeZone { timestamp: left, time_zone: right }
        | Expr::IsDistinctFrom(left, right)
        | Expr::IsNotDistinctFrom(left, right) => {
            first_function_name(left).or_else(|| first_function_name(right))
        }
        Expr::Tuple(exprs) | Expr::Array(Array { elem: exprs, .. }) => {
            exprs.iter().find_map(first_function_name)
        }
        Expr::InList { expr, list, .. } => {
            first_function_name(expr).or_else(|| list.iter().find_map(first_function_name))
        }
        Expr::Between { expr, low, high, .. } => {
            first_function_name(expr)
                .or_else(|| first_function_name(low))
                .or_else(|| first_function_name(high))
        }
        Expr::Case { operand, conditions, else_result, .. } => {
            operand
                .as_deref()
                .and_then(first_function_name)
                .or_else(|| {
                    conditions.iter().find_map(|case_when| {
                        first_function_name(&case_when.condition)
                            .or_else(|| first_function_name(&case_when.result))
                    })
                })
                .or_else(|| else_result.as_deref().and_then(first_function_name))
        }
        Expr::Trim { expr, trim_what, trim_characters, .. } => {
            first_function_name(expr)
                .or_else(|| trim_what.as_deref().and_then(first_function_name))
                .or_else(|| {
                    trim_characters
                        .as_deref()
                        .and_then(|chars| chars.iter().find_map(first_function_name))
                })
        }
        Expr::Substring { expr, substring_from, substring_for, .. } => {
            first_function_name(expr)
                .or_else(|| substring_from.as_deref().and_then(first_function_name))
                .or_else(|| substring_for.as_deref().and_then(first_function_name))
        }
        Expr::Overlay { expr, overlay_what, overlay_from, overlay_for } => {
            first_function_name(expr)
                .or_else(|| first_function_name(overlay_what))
                .or_else(|| first_function_name(overlay_from))
                .or_else(|| overlay_for.as_deref().and_then(first_function_name))
        }
        Expr::CompoundFieldAccess { root, access_chain } => {
            first_function_name(root).or_else(|| {
                access_chain.iter().find_map(|access| {
                    if let AccessExpr::Dot(expr) = access {
                        first_function_name(expr)
                    } else {
                        None
                    }
                })
            })
        }
        Expr::Interval(interval) => first_function_name(&interval.value),
        _ => None,
    }
}

impl Translator for TableConstraint {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Option<TableConstraint>;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        match self {
            Self::Check(check_constraint) => {
                match check_constraint.expr.translate(schema, options) {
                    Ok(translated_expr) => {
                        Ok(Some(Self::Check(sqlparser::ast::CheckConstraint {
                            name: check_constraint.name.clone(),
                            expr: Box::new(translated_expr),
                            enforced: check_constraint.enforced,
                        })))
                    }
                    Err(_) if options.should_remove_unsupported_check_constraints() => Ok(None),
                    Err(e) => {
                        // If there's a function name we can identify, wrap as
                        // UndefinedFunction for backwards-compatible error message.
                        if let Some(function_name) =
                            first_function_name(check_constraint.expr.as_ref())
                        {
                            Err(crate::errors::Error::UndefinedFunction(function_name))
                        } else {
                            Err(e)
                        }
                    }
                }
            }
            Self::ForeignKey(fk_constraint) => {
                // Check if the referenced table has RLS and update the foreign_table reference
                let mut updated_fk = fk_constraint.clone();

                if table_has_implicit_public_rls(schema, &fk_constraint.foreign_table)? {
                    updated_fk.foreign_table =
                        append_suffix(&fk_constraint.foreign_table, options.get_rls_table_suffix());
                }

                // Translate referential actions (consistent with column_option.rs)
                updated_fk.on_delete =
                    fk_constraint.on_delete.map(|a| a.translate(schema, options)).transpose()?;
                updated_fk.on_update =
                    fk_constraint.on_update.map(|a| a.translate(schema, options)).transpose()?;

                // Translate characteristics (consistent with column_option.rs:252-254)
                updated_fk.characteristics = fk_constraint
                    .characteristics
                    .map(|c| c.translate(schema, options))
                    .transpose()?;

                Ok(Some(Self::ForeignKey(updated_fk)))
            }
            Self::PrimaryKey(pk_constraint) => {
                let mut updated_pk = pk_constraint.clone();
                updated_pk.columns = pk_constraint
                    .columns
                    .iter()
                    .map(|col| col.translate(schema, options))
                    .collect::<Result<Vec<_>, _>>()?;
                updated_pk.characteristics = pk_constraint
                    .characteristics
                    .map(|c| c.translate(schema, options))
                    .transpose()?;
                Ok(Some(Self::PrimaryKey(updated_pk)))
            }
            Self::Unique(unique_constraint) => {
                let mut updated_unique = unique_constraint.clone();
                updated_unique.columns = unique_constraint
                    .columns
                    .iter()
                    .map(|col| col.translate(schema, options))
                    .collect::<Result<Vec<_>, _>>()?;
                updated_unique.characteristics = unique_constraint
                    .characteristics
                    .map(|c| c.translate(schema, options))
                    .transpose()?;
                Ok(Some(Self::Unique(updated_unique)))
            }
            other => Ok(Some(other.clone())),
        }
    }
}
