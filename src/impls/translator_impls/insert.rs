//! Implementation of the [`Translator`] trait for the
//! `Insert` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::Insert;

use crate::prelude::{Pg2SqliteOptions, Translator};

impl Translator for Insert {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Insert;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // Translate the source (VALUES or SELECT)
        let source =
            self.source.as_ref().map(|q| q.translate(schema, options)).transpose()?.map(Box::new);

        let mut insert = Insert { source, ..self.clone() };

        // Handle ON CONFLICT
        if let Some(on_insert) = &self.on {
            match on_insert {
                sqlparser::ast::OnInsert::OnConflict(on_conflict) => {
                    match &on_conflict.action {
                        sqlparser::ast::OnConflictAction::DoNothing => {
                            // SQLite uses INSERT OR IGNORE
                            insert.or = Some(sqlparser::ast::SqliteOnConflict::Ignore);
                            insert.on = None;
                        }
                        sqlparser::ast::OnConflictAction::DoUpdate(_) => {
                            // SQLite supports ON CONFLICT DO UPDATE with nearly
                            // identical syntax to PostgreSQL (since SQLite
                            // 3.24.0).
                            // EXCLUDED references work the same way
                            // (case-insensitive).
                            // Pass through as-is.
                        }
                    }
                }
                _ => unimplemented!("Unsupported ON INSERT: {:?}", on_insert),
            }
        }
        Ok(insert)
    }
}
