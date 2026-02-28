//! Implementation of the [`Translator`] trait for the
//! `Insert` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::Insert;

use super::helpers::Forward;
use crate::{
    impls::shared_helpers::{translate_on_conflict_do_update, translate_returning},
    prelude::{Pg2SqliteOptions, Translator},
};

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
        let returning = translate_returning::<Forward>(self.returning.as_ref(), schema, options)?;

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
                            insert.on = Some(translate_on_conflict_do_update::<Forward>(
                                on_conflict,
                                do_update,
                                schema,
                                options,
                            )?);
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

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{Insert, OnInsert, Statement},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use crate::prelude::{Pg2SqliteOptions, Translator};

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build")
    }

    fn parse_insert(sql: &str) -> Insert {
        let stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse").remove(0);
        let Statement::Insert(insert) = stmt else {
            panic!("expected insert");
        };
        insert
    }

    #[test]
    fn translate_rejects_non_on_conflict_insert_clause() {
        let mut insert = parse_insert("INSERT INTO users(id) VALUES (1)");
        insert.on = Some(OnInsert::DuplicateKeyUpdate(Vec::new()));

        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let err = insert
            .translate(&schema, &options)
            .expect_err("non-on-conflict ON INSERT clause should fail");

        assert!(err.to_string().contains("Unsupported ON INSERT clause"));
    }
}
