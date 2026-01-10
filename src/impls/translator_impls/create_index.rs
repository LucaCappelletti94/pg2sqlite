//! Implementation of the [`Translator`] trait for the
//! [`CreateIndex`](sqlparser::ast::CreateIndex) type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{CreateIndex, IndexType};

use crate::prelude::{Pg2SqliteOptions, Translator};

impl Translator for CreateIndex {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Option<Self>;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // If the index is a GIN or GiST index, we need to translate it into a table
        // with a FTS5 virtual table. This is because SQLite does not support
        // GIN or GiST indexes.
        if let Some(IndexType::GIN | IndexType::GiST) = self.using {
            todo!("Translate GIN/GiST index into FTS5 table");
            // let _fts5_table = create_fts5_from_index(self);
        }

        Ok(Some(CreateIndex {
            columns: self
                .columns
                .iter()
                .map(|col| col.translate(schema, options))
                .collect::<Result<_, _>>()?,
            predicate: self
                .predicate
                .as_ref()
                .map(|predicate| predicate.translate(schema, options))
                .transpose()?,
            ..self.clone()
        }))
    }
}
