//! Implementation of the [`Translator`] trait for the
//! `Query`, `SetExpr`, and `Select` types.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Query, Select, SetExpr, Values};

use super::helpers::{translate_select_item, translate_table_with_joins};
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
        // Translate ORDER BY expressions
        let order_by = self
            .order_by
            .as_ref()
            .map(|ob| -> Result<sqlparser::ast::OrderBy, crate::errors::Error> {
                let kind = match &ob.kind {
                    sqlparser::ast::OrderByKind::Expressions(exprs) => {
                        let translated_exprs = exprs
                            .iter()
                            .map(|expr| expr.translate(schema, options))
                            .collect::<Result<Vec<_>, _>>()?;
                        sqlparser::ast::OrderByKind::Expressions(translated_exprs)
                    }
                    sqlparser::ast::OrderByKind::All(all) => sqlparser::ast::OrderByKind::All(*all),
                };
                Ok(sqlparser::ast::OrderBy { kind, interpolate: ob.interpolate.clone() })
            })
            .transpose()?;

        Ok(Query {
            with: translate_with(self.with.as_ref(), schema, options)?,
            body: Box::new(self.body.translate(schema, options)?),
            order_by,
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
            SetExpr::Values(values) => SetExpr::Values(translate_values(values, schema, options)?),
            SetExpr::Insert(_)
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

        // Translate expressions in projections (SELECT clause)
        let projection = self
            .projection
            .iter()
            .map(|item| translate_select_item(item, schema, options))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Select {
            select_token: self.select_token.clone(),
            distinct: self.distinct.clone(),
            top: self.top.clone(),
            top_before_distinct: self.top_before_distinct,
            projection,
            into: self.into.clone(),
            from,
            lateral_views: self.lateral_views.clone(),
            prewhere: self.prewhere.clone(),
            selection,
            group_by: translate_group_by(&self.group_by, schema, options)?,
            cluster_by: self.cluster_by.clone(),
            distribute_by: self.distribute_by.clone(),
            sort_by: self.sort_by.clone(),
            having: self.having.as_ref().map(|expr| expr.translate(schema, options)).transpose()?,
            named_window: self.named_window.clone(),
            qualify: self.qualify.clone(),
            window_before_qualify: self.window_before_qualify,
            value_table_mode: self.value_table_mode,
            connect_by: self.connect_by.clone(),
            flavor: self.flavor,
            exclude: self.exclude.clone(),
            optimizer_hint: self.optimizer_hint.clone(),
            select_modifiers: self.select_modifiers.clone(),
        })
    }
}

fn translate_with(
    with: Option<&sqlparser::ast::With>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<sqlparser::ast::With>, crate::errors::Error> {
    with.map(|w| {
        let cte_tables = w
            .cte_tables
            .iter()
            .map(|cte| {
                Ok(sqlparser::ast::Cte {
                    alias: cte.alias.clone(),
                    query: Box::new(cte.query.translate(schema, options)?),
                    from: cte.from.clone(),
                    materialized: cte.materialized,
                    closing_paren_token: cte.closing_paren_token.clone(),
                })
            })
            .collect::<Result<Vec<_>, crate::errors::Error>>()?;
        Ok(sqlparser::ast::With {
            with_token: w.with_token.clone(),
            recursive: w.recursive,
            cte_tables,
        })
    })
    .transpose()
}

fn translate_group_by(
    group_by: &sqlparser::ast::GroupByExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::GroupByExpr, crate::errors::Error> {
    Ok(match group_by {
        sqlparser::ast::GroupByExpr::Expressions(exprs, modifiers) => {
            let translated = exprs
                .iter()
                .map(|e| e.translate(schema, options))
                .collect::<Result<Vec<_>, _>>()?;
            sqlparser::ast::GroupByExpr::Expressions(translated, modifiers.clone())
        }
        sqlparser::ast::GroupByExpr::All(all) => sqlparser::ast::GroupByExpr::All(all.clone()),
    })
}

fn translate_values(
    values: &Values,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Values, crate::errors::Error> {
    let translated_rows = values
        .rows
        .iter()
        .map(|row| {
            row.iter().map(|expr| expr.translate(schema, options)).collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Values {
        explicit_row: values.explicit_row,
        rows: translated_rows,
        value_keyword: values.value_keyword,
    })
}
