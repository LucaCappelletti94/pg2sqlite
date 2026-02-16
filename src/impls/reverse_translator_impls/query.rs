//! Implementation of the [`ReverseTranslator`] trait for the
//! `Query`, `SetExpr`, and `Select` types.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    Join, JoinConstraint, JoinOperator, Query, Select, SelectItem, SetExpr, TableFactor,
    TableWithJoins, Values,
};

use crate::{
    errors::Error,
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

impl ReverseTranslator for Query {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Query;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        // Reverse translate ORDER BY expressions
        let order_by = self
            .order_by
            .as_ref()
            .map(|ob| -> Result<sqlparser::ast::OrderBy, Error> {
                let kind = match &ob.kind {
                    sqlparser::ast::OrderByKind::Expressions(exprs) => {
                        let translated_exprs = exprs
                            .iter()
                            .map(|expr| reverse_translate_order_by_expr(expr, schema, options))
                            .collect::<Result<Vec<_>, _>>()?;
                        sqlparser::ast::OrderByKind::Expressions(translated_exprs)
                    }
                    sqlparser::ast::OrderByKind::All(all) => sqlparser::ast::OrderByKind::All(*all),
                };
                Ok(sqlparser::ast::OrderBy { kind, interpolate: ob.interpolate.clone() })
            })
            .transpose()?;

        Ok(Query {
            with: self.with.clone(),
            body: Box::new(self.body.reverse_translate(schema, options)?),
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

impl ReverseTranslator for SetExpr {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = SetExpr;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        Ok(match self {
            SetExpr::Select(select) => {
                SetExpr::Select(Box::new(select.reverse_translate(schema, options)?))
            }
            SetExpr::Query(query) => {
                SetExpr::Query(Box::new(query.reverse_translate(schema, options)?))
            }
            SetExpr::SetOperation { op, set_quantifier, left, right } => {
                SetExpr::SetOperation {
                    op: *op,
                    set_quantifier: *set_quantifier,
                    left: Box::new(left.reverse_translate(schema, options)?),
                    right: Box::new(right.reverse_translate(schema, options)?),
                }
            }
            SetExpr::Values(values) => {
                SetExpr::Values(reverse_translate_values(values, schema, options)?)
            }
            SetExpr::Insert(_)
            | SetExpr::Table(_)
            | SetExpr::Update(_)
            | SetExpr::Delete(_)
            | SetExpr::Merge(_) => self.clone(),
        })
    }
}

impl ReverseTranslator for Select {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Select;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        // Reverse translate the WHERE clause
        let selection = self
            .selection
            .as_ref()
            .map(|expr| expr.reverse_translate(schema, options))
            .transpose()?;

        // Reverse translate HAVING clause
        let having =
            self.having.as_ref().map(|expr| expr.reverse_translate(schema, options)).transpose()?;

        // Reverse translate subqueries in FROM clause
        let from = self
            .from
            .iter()
            .map(|table_with_joins| {
                reverse_translate_table_with_joins(table_with_joins, schema, options)
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Reverse translate expressions in projections
        let projection = self
            .projection
            .iter()
            .map(|item| reverse_translate_select_item(item, schema, options))
            .collect::<Result<Vec<_>, _>>()?;

        // Reverse translate GROUP BY expressions
        let group_by = reverse_translate_group_by(&self.group_by, schema, options)?;

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
            group_by,
            cluster_by: self.cluster_by.clone(),
            distribute_by: self.distribute_by.clone(),
            sort_by: self.sort_by.clone(),
            having,
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

fn reverse_translate_table_with_joins(
    table_with_joins: &TableWithJoins,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableWithJoins, Error> {
    let translated_joins = table_with_joins
        .joins
        .iter()
        .map(|join| reverse_translate_join(join, schema, options))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(TableWithJoins {
        relation: reverse_translate_table_factor(&table_with_joins.relation, schema, options)?,
        joins: translated_joins,
    })
}

fn reverse_translate_join(
    join: &Join,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Join, Error> {
    Ok(Join {
        relation: reverse_translate_table_factor(&join.relation, schema, options)?,
        global: join.global,
        join_operator: reverse_translate_join_operator(&join.join_operator, schema, options)?,
    })
}

fn reverse_translate_join_operator(
    join_operator: &JoinOperator,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<JoinOperator, Error> {
    Ok(match join_operator {
        JoinOperator::Join(constraint) => {
            JoinOperator::Join(reverse_translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::Inner(constraint) => {
            JoinOperator::Inner(reverse_translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::Left(constraint) => {
            JoinOperator::Left(reverse_translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::LeftOuter(constraint) => {
            JoinOperator::LeftOuter(reverse_translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::Right(constraint) => {
            JoinOperator::Right(reverse_translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::RightOuter(constraint) => {
            JoinOperator::RightOuter(reverse_translate_join_constraint(
                constraint, schema, options,
            )?)
        }
        JoinOperator::FullOuter(constraint) => {
            JoinOperator::FullOuter(reverse_translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::CrossJoin(constraint) => {
            JoinOperator::CrossJoin(reverse_translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::Semi(constraint) => {
            JoinOperator::Semi(reverse_translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::LeftSemi(constraint) => {
            JoinOperator::LeftSemi(reverse_translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::RightSemi(constraint) => {
            JoinOperator::RightSemi(reverse_translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::Anti(constraint) => {
            JoinOperator::Anti(reverse_translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::LeftAnti(constraint) => {
            JoinOperator::LeftAnti(reverse_translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::RightAnti(constraint) => {
            JoinOperator::RightAnti(reverse_translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::AsOf { constraint, match_condition } => {
            JoinOperator::AsOf {
                constraint: reverse_translate_join_constraint(constraint, schema, options)?,
                match_condition: match_condition.reverse_translate(schema, options)?,
            }
        }
        JoinOperator::StraightJoin(constraint) => {
            JoinOperator::StraightJoin(reverse_translate_join_constraint(
                constraint, schema, options,
            )?)
        }
        JoinOperator::CrossApply | JoinOperator::OuterApply => join_operator.clone(),
    })
}

fn reverse_translate_join_constraint(
    constraint: &JoinConstraint,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<JoinConstraint, Error> {
    Ok(match constraint {
        JoinConstraint::On(expr) => JoinConstraint::On(expr.reverse_translate(schema, options)?),
        JoinConstraint::Using(idents) => JoinConstraint::Using(idents.clone()),
        JoinConstraint::Natural => JoinConstraint::Natural,
        JoinConstraint::None => JoinConstraint::None,
    })
}

fn reverse_translate_table_factor(
    table_factor: &TableFactor,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableFactor, Error> {
    Ok(match table_factor {
        TableFactor::Derived { subquery, lateral, alias, sample } => {
            TableFactor::Derived {
                subquery: Box::new(subquery.reverse_translate(schema, options)?),
                lateral: *lateral,
                alias: alias.clone(),
                sample: sample.clone(),
            }
        }
        TableFactor::NestedJoin { table_with_joins, alias } => {
            TableFactor::NestedJoin {
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
    item: &SelectItem,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<SelectItem, Error> {
    Ok(match item {
        SelectItem::UnnamedExpr(expr) => {
            SelectItem::UnnamedExpr(expr.reverse_translate(schema, options)?)
        }
        SelectItem::ExprWithAlias { expr, alias } => {
            SelectItem::ExprWithAlias {
                expr: expr.reverse_translate(schema, options)?,
                alias: alias.clone(),
            }
        }
        other => other.clone(),
    })
}

fn reverse_translate_values(
    values: &Values,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Values, Error> {
    let translated_rows = values
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|expr| expr.reverse_translate(schema, options))
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Values {
        explicit_row: values.explicit_row,
        rows: translated_rows,
        value_keyword: values.value_keyword,
    })
}

fn reverse_translate_order_by_expr(
    expr: &sqlparser::ast::OrderByExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::OrderByExpr, Error> {
    Ok(sqlparser::ast::OrderByExpr {
        expr: expr.expr.reverse_translate(schema, options)?,
        options: expr.options,
        with_fill: expr.with_fill.clone(),
    })
}

fn reverse_translate_group_by(
    group_by: &sqlparser::ast::GroupByExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::GroupByExpr, Error> {
    Ok(match group_by {
        sqlparser::ast::GroupByExpr::Expressions(exprs, modifiers) => {
            let translated_exprs = exprs
                .iter()
                .map(|e| e.reverse_translate(schema, options))
                .collect::<Result<Vec<_>, _>>()?;
            sqlparser::ast::GroupByExpr::Expressions(translated_exprs, modifiers.clone())
        }
        sqlparser::ast::GroupByExpr::All(all) => sqlparser::ast::GroupByExpr::All(all.clone()),
    })
}
