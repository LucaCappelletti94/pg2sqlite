//! Implementation of the [`Translator`] trait for the
//! `Query`, `SetExpr`, and `Select` types.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Query, Select, SetExpr, TableFactor, TableWithJoins};

use crate::prelude::{Pg2SqliteOptions, Translator};

impl Translator for Query {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Query;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        Ok(Query {
            with: self.with.clone(), // CTEs - pass through for now
            body: Box::new(self.body.translate(schema, options)?),
            order_by: self.order_by.clone(),
            limit_clause: self.limit_clause.clone(),
            fetch: self.fetch.clone(),
            locks: self.locks.clone(),
            for_clause: self.for_clause.clone(),
            settings: self.settings.clone(),
            format_clause: self.format_clause.clone(),
            pipe_operators: self.pipe_operators.clone(),
        })
    }
}

impl Translator for SetExpr {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = SetExpr;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        Ok(match self {
            SetExpr::Select(select) => {
                SetExpr::Select(Box::new(select.translate(schema, options)?))
            }
            SetExpr::Query(query) => SetExpr::Query(Box::new(query.translate(schema, options)?)),
            SetExpr::SetOperation { op, set_quantifier, left, right } => {
                SetExpr::SetOperation {
                    op: *op,
                    set_quantifier: *set_quantifier,
                    left: Box::new(left.translate(schema, options)?),
                    right: Box::new(right.translate(schema, options)?),
                }
            }
            SetExpr::Values(_)
            | SetExpr::Insert(_)
            | SetExpr::Table(_)
            | SetExpr::Update(_)
            | SetExpr::Delete(_)
            | SetExpr::Merge(_) => self.clone(),
        })
    }
}

impl Translator for Select {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Select;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // Translate the WHERE clause (selection) which may contain @@ expressions
        let selection =
            self.selection.as_ref().map(|expr| expr.translate(schema, options)).transpose()?;

        // Translate subqueries in FROM clause
        let from = self
            .from
            .iter()
            .map(|table_with_joins| translate_table_with_joins(table_with_joins, schema, options))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Select {
            select_token: self.select_token.clone(),
            distinct: self.distinct.clone(),
            top: self.top.clone(),
            top_before_distinct: self.top_before_distinct,
            projection: self.projection.clone(), // TODO: translate expressions in projections
            into: self.into.clone(),
            from,
            lateral_views: self.lateral_views.clone(),
            prewhere: self.prewhere.clone(),
            selection,
            group_by: self.group_by.clone(),
            cluster_by: self.cluster_by.clone(),
            distribute_by: self.distribute_by.clone(),
            sort_by: self.sort_by.clone(),
            having: self.having.clone(),
            named_window: self.named_window.clone(),
            qualify: self.qualify.clone(),
            window_before_qualify: self.window_before_qualify,
            value_table_mode: self.value_table_mode,
            connect_by: self.connect_by.clone(),
            flavor: self.flavor,
            exclude: self.exclude.clone(),
            optimizer_hint: self.optimizer_hint.clone(),
        })
    }
}

fn translate_table_with_joins(
    table_with_joins: &TableWithJoins,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableWithJoins, crate::errors::Error> {
    Ok(TableWithJoins {
        relation: translate_table_factor(&table_with_joins.relation, schema, options)?,
        joins: table_with_joins.joins.clone(), // TODO: translate join conditions
    })
}

fn translate_table_factor(
    table_factor: &TableFactor,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableFactor, crate::errors::Error> {
    Ok(match table_factor {
        TableFactor::Derived { subquery, lateral, alias, sample } => {
            TableFactor::Derived {
                subquery: Box::new(subquery.translate(schema, options)?),
                lateral: *lateral,
                alias: alias.clone(),
                sample: sample.clone(),
            }
        }
        TableFactor::NestedJoin { table_with_joins, alias } => {
            TableFactor::NestedJoin {
                table_with_joins: Box::new(translate_table_with_joins(
                    table_with_joins,
                    schema,
                    options,
                )?),
                alias: alias.clone(),
            }
        }
        // Pass through other table factors unchanged
        other => other.clone(),
    })
}
