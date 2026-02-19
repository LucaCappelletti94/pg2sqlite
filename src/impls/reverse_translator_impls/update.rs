//! Implementation of the [`ReverseTranslator`] trait for the
//! `Update` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Update, UpdateTableFromKind};

use super::helpers::{Reverse, reverse_translate_table_with_joins};
use crate::{
    errors::Error,
    impls::shared_helpers::translate_returning,
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

impl ReverseTranslator for Update {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Update;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        // Reverse translate SET clause expressions
        let assignments = self
            .assignments
            .iter()
            .map(|assignment| {
                Ok(sqlparser::ast::Assignment {
                    target: assignment.target.clone(),
                    value: assignment.value.reverse_translate(schema, options)?,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        // Reverse translate WHERE clause
        let selection = self
            .selection
            .as_ref()
            .map(|expr| expr.reverse_translate(schema, options))
            .transpose()?;

        // Reverse translate FROM clause if present
        let from = self
            .from
            .as_ref()
            .map(|f| reverse_translate_update_from(f, schema, options))
            .transpose()?;

        // Reverse translate RETURNING clause if present
        let returning = translate_returning::<Reverse>(self.returning.as_ref(), schema, options)?;

        // Reverse translate the table
        let table = reverse_translate_table_with_joins(&self.table, schema, options)?;

        Ok(Update {
            update_token: self.update_token.clone(),
            optimizer_hint: self.optimizer_hint.clone(),
            table,
            assignments,
            from,
            selection,
            returning,
            or: self.or,
            limit: self.limit.clone(),
        })
    }
}

fn reverse_translate_update_from(
    from: &UpdateTableFromKind,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<UpdateTableFromKind, Error> {
    Ok(match from {
        UpdateTableFromKind::BeforeSet(tables) => {
            UpdateTableFromKind::BeforeSet(
                tables
                    .iter()
                    .map(|t| reverse_translate_table_with_joins(t, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        UpdateTableFromKind::AfterSet(tables) => {
            UpdateTableFromKind::AfterSet(
                tables
                    .iter()
                    .map(|t| reverse_translate_table_with_joins(t, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
    })
}
