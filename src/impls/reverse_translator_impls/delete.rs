//! Implementation of the [`ReverseTranslator`] trait for the
//! `Delete` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Delete, FromTable};

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

fn reverse_translate_table_with_joins(
    table_with_joins: &sqlparser::ast::TableWithJoins,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::TableWithJoins, Error> {
    let translated_joins = table_with_joins
        .joins
        .iter()
        .map(|join| reverse_translate_join(join, schema, options))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(sqlparser::ast::TableWithJoins {
        relation: reverse_translate_table_factor(&table_with_joins.relation, schema, options)?,
        joins: translated_joins,
    })
}

fn reverse_translate_join(
    join: &sqlparser::ast::Join,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::Join, Error> {
    Ok(sqlparser::ast::Join {
        relation: reverse_translate_table_factor(&join.relation, schema, options)?,
        global: join.global,
        join_operator: join.join_operator.clone(),
    })
}

fn reverse_translate_table_factor(
    table_factor: &sqlparser::ast::TableFactor,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::TableFactor, Error> {
    Ok(match table_factor {
        sqlparser::ast::TableFactor::Derived { subquery, lateral, alias, sample } => {
            sqlparser::ast::TableFactor::Derived {
                subquery: Box::new(subquery.reverse_translate(schema, options)?),
                lateral: *lateral,
                alias: alias.clone(),
                sample: sample.clone(),
            }
        }
        sqlparser::ast::TableFactor::NestedJoin { table_with_joins, alias } => {
            sqlparser::ast::TableFactor::NestedJoin {
                table_with_joins: Box::new(reverse_translate_table_with_joins(
                    table_with_joins,
                    schema,
                    options,
                )?),
                alias: alias.clone(),
            }
        }
        other => other.clone(),
    })
}

fn reverse_translate_select_item(
    item: &sqlparser::ast::SelectItem,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::SelectItem, Error> {
    Ok(match item {
        sqlparser::ast::SelectItem::UnnamedExpr(expr) => {
            sqlparser::ast::SelectItem::UnnamedExpr(expr.reverse_translate(schema, options)?)
        }
        sqlparser::ast::SelectItem::ExprWithAlias { expr, alias } => {
            sqlparser::ast::SelectItem::ExprWithAlias {
                expr: expr.reverse_translate(schema, options)?,
                alias: alias.clone(),
            }
        }
        other => other.clone(),
    })
}
