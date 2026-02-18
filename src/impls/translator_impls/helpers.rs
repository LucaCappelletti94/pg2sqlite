//! Shared helper functions for forward translation of table references,
//! joins, and select items.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Join, JoinConstraint, JoinOperator, SelectItem, TableFactor, TableWithJoins};

use crate::{
    errors::Error,
    prelude::{Pg2SqliteOptions, Translator},
};

pub(super) fn translate_table_with_joins(
    table_with_joins: &TableWithJoins,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableWithJoins, Error> {
    let translated_joins = table_with_joins
        .joins
        .iter()
        .map(|join| translate_join(join, schema, options))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(TableWithJoins {
        relation: translate_table_factor(&table_with_joins.relation, schema, options)?,
        joins: translated_joins,
    })
}

pub(super) fn translate_join(
    join: &Join,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Join, Error> {
    Ok(Join {
        relation: translate_table_factor(&join.relation, schema, options)?,
        global: join.global,
        join_operator: translate_join_operator(&join.join_operator, schema, options)?,
    })
}

pub(super) fn translate_join_operator(
    join_operator: &JoinOperator,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<JoinOperator, Error> {
    Ok(match join_operator {
        JoinOperator::Join(constraint) => {
            JoinOperator::Join(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::Inner(constraint) => {
            JoinOperator::Inner(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::Left(constraint) => {
            JoinOperator::Left(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::LeftOuter(constraint) => {
            JoinOperator::LeftOuter(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::Right(constraint) => {
            JoinOperator::Right(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::RightOuter(constraint) => {
            JoinOperator::RightOuter(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::FullOuter(constraint) => {
            JoinOperator::FullOuter(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::CrossJoin(constraint) => {
            JoinOperator::CrossJoin(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::Semi(constraint) => {
            JoinOperator::Semi(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::LeftSemi(constraint) => {
            JoinOperator::LeftSemi(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::RightSemi(constraint) => {
            JoinOperator::RightSemi(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::Anti(constraint) => {
            JoinOperator::Anti(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::LeftAnti(constraint) => {
            JoinOperator::LeftAnti(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::RightAnti(constraint) => {
            JoinOperator::RightAnti(translate_join_constraint(constraint, schema, options)?)
        }
        JoinOperator::AsOf { constraint, match_condition } => {
            JoinOperator::AsOf {
                constraint: translate_join_constraint(constraint, schema, options)?,
                match_condition: match_condition.translate(schema, options)?,
            }
        }
        JoinOperator::StraightJoin(constraint) => {
            JoinOperator::StraightJoin(translate_join_constraint(constraint, schema, options)?)
        }
        // These operators don't have constraints that need translation
        JoinOperator::CrossApply | JoinOperator::OuterApply => join_operator.clone(),
    })
}

pub(super) fn translate_join_constraint(
    constraint: &JoinConstraint,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<JoinConstraint, Error> {
    Ok(match constraint {
        JoinConstraint::On(expr) => JoinConstraint::On(expr.translate(schema, options)?),
        // USING and other constraints don't need expression translation
        JoinConstraint::Using(idents) => JoinConstraint::Using(idents.clone()),
        JoinConstraint::Natural => JoinConstraint::Natural,
        JoinConstraint::None => JoinConstraint::None,
    })
}

pub(super) fn translate_table_factor(
    table_factor: &TableFactor,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableFactor, Error> {
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

pub(super) fn translate_select_item(
    item: &SelectItem,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<SelectItem, Error> {
    Ok(match item {
        SelectItem::UnnamedExpr(expr) => SelectItem::UnnamedExpr(expr.translate(schema, options)?),
        SelectItem::ExprWithAlias { expr, alias } => {
            SelectItem::ExprWithAlias {
                expr: expr.translate(schema, options)?,
                alias: alias.clone(),
            }
        }
        // Wildcards and qualified wildcards don't need translation
        other => other.clone(),
    })
}
