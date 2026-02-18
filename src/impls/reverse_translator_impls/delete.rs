//! Implementation of the [`ReverseTranslator`] trait for the
//! `Delete` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Delete, FromTable};

use super::helpers::{reverse_translate_select_item, reverse_translate_table_with_joins};
use crate::{
    errors::Error,
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

impl ReverseTranslator for Delete {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Delete;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        // Reverse translate WHERE clause
        let selection = self
            .selection
            .as_ref()
            .map(|expr| expr.reverse_translate(schema, options))
            .transpose()?;

        // Reverse translate FROM clause
        let from = reverse_translate_from_table(&self.from, schema, options)?;

        // Reverse translate USING clause if present
        let using = self
            .using
            .as_ref()
            .map(|tables| {
                tables
                    .iter()
                    .map(|table_with_joins| {
                        reverse_translate_table_with_joins(table_with_joins, schema, options)
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        // Reverse translate RETURNING clause if present
        let returning = self
            .returning
            .as_ref()
            .map(|items| {
                items
                    .iter()
                    .map(|item| reverse_translate_select_item(item, schema, options))
                    .collect::<Result<Vec<_>, Error>>()
            })
            .transpose()?;

        Ok(Delete {
            delete_token: self.delete_token.clone(),
            optimizer_hint: self.optimizer_hint.clone(),
            tables: self.tables.clone(),
            from,
            using,
            selection,
            returning,
            order_by: self.order_by.clone(),
            limit: self.limit.clone(),
        })
    }
}

fn reverse_translate_from_table(
    from: &FromTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<FromTable, Error> {
    Ok(match from {
        FromTable::WithFromKeyword(tables) => {
            FromTable::WithFromKeyword(
                tables
                    .iter()
                    .map(|t| reverse_translate_table_with_joins(t, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        FromTable::WithoutKeyword(tables) => {
            FromTable::WithoutKeyword(
                tables
                    .iter()
                    .map(|t| reverse_translate_table_with_joins(t, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
    })
}
