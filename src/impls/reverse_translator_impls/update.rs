//! Implementation of the [`ReverseTranslator`] trait for the
//! `Update` type.

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
use sqlparser::ast::{AssignmentTarget, Expr, ObjectName, TableFactor, Update};

use super::helpers::{Reverse, unscale_integer_literal};
use crate::{
    errors::Error,
    impls::{
        object_name::{last_ident, resolve_translation_table},
        shared_helpers::{numeric_minor_unit_scales_of_table, translate_update},
        translator_impls::update::update_scope_query,
    },
    prelude::ReverseTranslator,
};

/// Unscales an assignment value when its target column holds minor units.
fn unscale_assignment(target: &AssignmentTarget, value: &mut Expr, scales: &[(String, u32)]) {
    match target {
        AssignmentTarget::ColumnName(name) => unscale_for_column(name, value, scales),
        AssignmentTarget::Tuple(names) => {
            let Expr::Tuple(items) = value else { return };
            if items.len() != names.len() {
                return;
            }
            for (name, item) in names.iter().zip(items.iter_mut()) {
                unscale_for_column(name, item, scales);
            }
        }
    }
}

/// Unscales `value` when `name` names a column carrying a scale.
fn unscale_for_column(name: &ObjectName, value: &mut Expr, scales: &[(String, u32)]) {
    if let Some(column) = last_ident(name)
        && let Some(scale) = scales
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(&column.value))
            .map(|(_, scale)| *scale)
        && let Some(unscaled) = unscale_integer_literal(value, scale)
    {
        *value = unscaled;
    }
}

impl ReverseTranslator for Update {
    type Schema = ParserDB;
    type PostgresEntry = Update;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<Self::PostgresEntry, Error> {
        // PostgreSQL UPDATE has no conflict-resolution clause at all. Each
        // SQLite OR mode changes error handling in a distinct way
        // (ROLLBACK reverts the transaction, ABORT stops the statement
        // but keeps prior changes, FAIL stops at the first conflicting
        // row, IGNORE skips conflicting rows, REPLACE deletes and
        // re-inserts), so silently dropping the clause would change the
        // observable error behaviour. Reject all of them.
        if let Some(or_clause) = self.or {
            return Err(Error::reverse_refusal(format!(
                "UPDATE {or_clause} has no PostgreSQL form. PostgreSQL UPDATE has no \
                 conflict-resolution clause. Use an explicit transaction with appropriate \
                 error handling instead."
            )));
        }
        // PostgreSQL UPDATE has no ORDER BY or LIMIT clause. Refuse them so
        // the emitted SQL does not fail at the server with a syntax error.
        if !self.order_by.is_empty() || self.limit.is_some() {
            return Err(Error::reverse_refusal(
                "PostgreSQL UPDATE has no ORDER BY or LIMIT clause; these are SQLite extensions \
             with no PostgreSQL form"
                    .to_string(),
            ));
        }
        let scope_query = update_scope_query(self);
        let scope = sql_traits::structs::ColumnScope::from_query(&scope_query, schema)?;
        let scoped = options.with_scope(&scope);
        let mut translated = translate_update::<Reverse>(self, schema, &scoped, &mut |_| {})?;
        // Unscale integer literals assigned to NUMERIC columns.
        if let TableFactor::Table { name, .. } = &self.table.relation
            && let Ok(Some(table)) = resolve_translation_table(schema, name)
        {
            let scales = numeric_minor_unit_scales_of_table(table, schema);
            if !scales.is_empty() {
                for assignment in &mut translated.assignments {
                    unscale_assignment(&assignment.target, &mut assignment.value, &scales);
                }
            }
        }
        Ok(translated)
    }
}
