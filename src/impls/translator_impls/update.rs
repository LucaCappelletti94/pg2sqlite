//! Implementation of the [`Translator`] trait for the
//! `Update` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Update, UpdateTableFromKind};

use super::helpers::{Forward, translate_table_with_joins};
use crate::{
    errors::Error,
    impls::shared_helpers::translate_returning,
    prelude::{Pg2SqliteOptions, Translator},
};

impl Translator for Update {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Update;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, Error> {
        // SQLite does not support join syntax directly on the UPDATE target table.
        if !self.table.joins.is_empty() {
            return Err(Error::UnsupportedSQLiteFeature(
                "UPDATE with joins on the target table is not supported in SQLite. \
                 Use UPDATE ... FROM ... instead."
                    .to_string(),
            ));
        }

        let assignments = self
            .assignments
            .iter()
            .map(|assignment| {
                Ok(sqlparser::ast::Assignment {
                    target: assignment.target.clone(),
                    value: assignment.value.translate(schema, options)?,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        let selection =
            self.selection.as_ref().map(|expr| expr.translate(schema, options)).transpose()?;

        let from =
            self.from.as_ref().map(|f| translate_update_from(f, schema, options)).transpose()?;

        let returning = translate_returning::<Forward>(self.returning.as_ref(), schema, options)?;

        Ok(Update {
            update_token: self.update_token.clone(),
            optimizer_hint: self.optimizer_hint.clone(),
            table: translate_table_with_joins(&self.table, schema, options)?,
            assignments,
            from,
            selection,
            returning,
            or: self.or,
            limit: self.limit.clone(),
        })
    }
}

fn translate_update_from(
    from: &UpdateTableFromKind,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<UpdateTableFromKind, Error> {
    Ok(match from {
        UpdateTableFromKind::BeforeSet(tables) => {
            UpdateTableFromKind::BeforeSet(
                tables
                    .iter()
                    .map(|t| translate_table_with_joins(t, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        UpdateTableFromKind::AfterSet(tables) => {
            UpdateTableFromKind::AfterSet(
                tables
                    .iter()
                    .map(|t| translate_table_with_joins(t, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
    })
}
