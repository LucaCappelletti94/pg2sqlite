//! Implementation of the [`ReverseTranslator`] trait for the
//! `Delete` type.

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
use sqlparser::ast::Delete;

use super::helpers::{Reverse, reverse_translate_table_with_joins};
use crate::{
    errors::Error,
    impls::{shared_helpers::translate_delete_core, translator_impls::delete::delete_scope_query},
    prelude::ReverseTranslator,
};

impl ReverseTranslator for Delete {
    type Schema = ParserDB;
    type PostgresEntry = Delete;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<Self::PostgresEntry, Error> {
        // PostgreSQL DELETE has no ORDER BY or LIMIT clause. Refuse them so
        // the emitted SQL does not fail at the server with a syntax error.
        if !self.order_by.is_empty() || self.limit.is_some() {
            return Err(Error::reverse_refusal(
                "PostgreSQL DELETE has no ORDER BY or LIMIT clause; these are SQLite extensions \
             with no PostgreSQL form"
                    .to_string(),
            ));
        }
        let scope_query = delete_scope_query(self);
        let scope = sql_traits::structs::ColumnScope::from_query(&scope_query, schema)?;
        let scoped = options.with_scope(&scope);
        let (selection, from, returning, order_by, limit) =
            translate_delete_core::<Reverse>(self, schema, &scoped, &mut |_| {})?;

        // Reverse translate USING clause if present
        let using = self
            .using
            .as_ref()
            .map(|tables| {
                tables
                    .iter()
                    .map(|table_with_joins| {
                        reverse_translate_table_with_joins(
                            table_with_joins,
                            schema,
                            &scoped,
                            &mut |_| {},
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        Ok(Delete {
            delete_token: self.delete_token.clone(),
            optimizer_hints: self.optimizer_hints.clone(),
            tables: self.tables.clone(),
            from,
            using,
            selection,
            returning,
            output: self.output.clone(),
            order_by,
            limit,
        })
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{FromTable, Statement},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use crate::prelude::{Pg2SqliteOptions, ReverseTranslator};

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap()
    }

    fn parse_delete(sql: &str) -> sqlparser::ast::Delete {
        let stmt = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap().remove(0);
        match stmt {
            Statement::Delete(delete) => delete,
            other => panic!("expected delete, got: {other:?}"),
        }
    }

    #[test]
    fn reverse_translate_delete_handles_using_and_returning() {
        let delete = parse_delete(
            "DELETE FROM users USING accounts WHERE users.account_id = accounts.id RETURNING users.id",
        );
        let schema = empty_schema();
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());

        let translated = delete.reverse_translate(&schema, &options).unwrap();
        assert!(translated.using.is_some());
        assert!(translated.returning.is_some());
    }

    #[test]
    fn reverse_translate_from_table_supports_without_keyword_variant() {
        let mut delete = parse_delete("DELETE FROM users WHERE id = 1");
        let tables = match delete.from {
            FromTable::WithFromKeyword(ref tables) | FromTable::WithoutKeyword(ref tables) => {
                tables.clone()
            }
        };
        delete.from = FromTable::WithoutKeyword(tables);

        let schema = empty_schema();
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
        let translated = delete.reverse_translate(&schema, &options).unwrap();
        assert!(matches!(translated.from, FromTable::WithoutKeyword(_)));
    }
}
