//! Implementation of the [`ReverseTranslator`] trait for the
//! `Delete` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Delete, FromTable};

use super::helpers::{
    Reverse, reverse_translate_order_by_expr, reverse_translate_table_with_joins,
};
use crate::{
    errors::Error,
    impls::shared_helpers::translate_returning,
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
        let returning = translate_returning::<Reverse>(self.returning.as_ref(), schema, options)?;
        let order_by = self
            .order_by
            .iter()
            .map(|expr| reverse_translate_order_by_expr(expr, schema, options))
            .collect::<Result<Vec<_>, _>>()?;
        let limit =
            self.limit.as_ref().map(|expr| expr.reverse_translate(schema, options)).transpose()?;

        Ok(Delete {
            delete_token: self.delete_token.clone(),
            optimizer_hint: self.optimizer_hint.clone(),
            tables: self.tables.clone(),
            from,
            using,
            selection,
            returning,
            order_by,
            limit,
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

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{FromTable, Statement},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::reverse_translate_from_table;
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
        let options = Pg2SqliteOptions::default();

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
        let options = Pg2SqliteOptions::default();
        let translated = reverse_translate_from_table(&delete.from, &schema, &options).unwrap();
        assert!(matches!(translated, FromTable::WithoutKeyword(_)));
    }
}
