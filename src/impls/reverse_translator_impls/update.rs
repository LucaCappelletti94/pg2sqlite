//! Implementation of the [`ReverseTranslator`] trait for the
//! `Update` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Update, UpdateTableFromKind};

use crate::{
    errors::Error,
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

impl ReverseTranslator for Update {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Update;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        // Reverse translate SET clause expressions
        let assignments = self
            .assignments
            .iter()
            .map(|assignment| {
                Ok(sqlparser::ast::Assignment {
                    target: assignment.target.clone(),
                    value: assignment.value.reverse_translate(schema, options)?,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        // Reverse translate WHERE clause
        let selection = self
            .selection
            .as_ref()
            .map(|expr| expr.reverse_translate(schema, options))
            .transpose()?;

        // Reverse translate FROM clause if present
        let from = self
            .from
            .as_ref()
            .map(|f| reverse_translate_update_from(f, schema, options))
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

        // Reverse translate the table
        let table = reverse_translate_table_with_joins(&self.table, schema, options)?;

        Ok(Update {
            update_token: self.update_token.clone(),
            optimizer_hint: self.optimizer_hint.clone(),
            table,
            assignments,
            from,
            selection,
            returning,
            or: self.or,
            limit: self.limit.clone(),
        })
    }
}

fn reverse_translate_update_from(
    from: &UpdateTableFromKind,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<UpdateTableFromKind, Error> {
    Ok(match from {
        UpdateTableFromKind::BeforeSet(tables) => {
            UpdateTableFromKind::BeforeSet(
                tables
                    .iter()
                    .map(|t| reverse_translate_table_with_joins(t, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        UpdateTableFromKind::AfterSet(tables) => {
            UpdateTableFromKind::AfterSet(
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
