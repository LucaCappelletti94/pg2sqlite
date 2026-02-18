//! Shared helper functions for reverse translation of table references,
//! joins, and select items.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Join, JoinConstraint, JoinOperator, SelectItem, TableFactor, TableWithJoins};

use crate::{
    errors::Error,
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

pub(super) fn reverse_translate_table_with_joins(
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

pub(super) fn reverse_translate_join(
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

pub(super) fn reverse_translate_join_operator(
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

pub(super) fn reverse_translate_join_constraint(
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

pub(super) fn reverse_translate_table_factor(
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

pub(super) fn reverse_translate_select_item(
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
