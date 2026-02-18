//! Implementation of the [`Translator`] trait for the
//! `Insert` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::Insert;

use super::helpers::translate_select_item;
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

        // Translate RETURNING expressions
        let returning = self
            .returning
            .as_ref()
            .map(|items| {
                items
                    .iter()
                    .map(|item| translate_select_item(item, schema, options))
                    .collect::<Result<Vec<_>, crate::errors::Error>>()
            })
            .transpose()?;

        let mut insert = Insert { source, returning, ..self.clone() };

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
                        sqlparser::ast::OnConflictAction::DoUpdate(do_update) => {
                            // SQLite supports ON CONFLICT DO UPDATE with nearly
                            // identical syntax to PostgreSQL (since SQLite
                            // 3.24.0).
                            // EXCLUDED references work the same way
                            // (case-insensitive).
                            // Translate expressions in assignments and selection.
                            let translated_assignments = do_update
                                .assignments
                                .iter()
                                .map(|a| {
                                    Ok(sqlparser::ast::Assignment {
                                        target: a.target.clone(),
                                        value: a.value.translate(schema, options)?,
                                    })
                                })
                                .collect::<Result<Vec<_>, crate::errors::Error>>()?;
                            let translated_selection = do_update
                                .selection
                                .as_ref()
                                .map(|expr| expr.translate(schema, options))
                                .transpose()?;
                            insert.on = Some(sqlparser::ast::OnInsert::OnConflict(
                                sqlparser::ast::OnConflict {
                                    conflict_target: on_conflict.conflict_target.clone(),
                                    action: sqlparser::ast::OnConflictAction::DoUpdate(
                                        sqlparser::ast::DoUpdate {
                                            assignments: translated_assignments,
                                            selection: translated_selection,
                                        },
                                    ),
                                },
                            ));
                        }
                    }
                }
                _ => {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                        "Unsupported ON INSERT clause: {on_insert:?}"
                    )));
                }
            }
        }
        Ok(insert)
    }
}
