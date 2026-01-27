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
        _schema: &Self::Schema,
        _options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // For now, assume INSERT is compatible, but handle ON CONFLICT
        let mut insert = self.clone();
        if let Some(on_insert) = &self.on {
            match on_insert {
                sqlparser::ast::OnInsert::OnConflict(on_conflict) => {
                    match on_conflict.action {
                        sqlparser::ast::OnConflictAction::DoNothing => {
                            // SQLite uses INSERT OR IGNORE
                            insert.or = Some(sqlparser::ast::SqliteOnConflict::Ignore);
                            insert.on = None;
                        }
                        sqlparser::ast::OnConflictAction::DoUpdate(_) => {
                            unimplemented!(
                                "Unsupported ON CONFLICT action: {:?}",
                                on_conflict.action
                            )
                        }
                    }
                }
                _ => unimplemented!("Unsupported ON INSERT: {:?}", on_insert),
            }
        }
        Ok(insert)
    }
}
