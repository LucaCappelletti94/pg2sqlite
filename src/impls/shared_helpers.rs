//! Shared helper functions for translating table references, joins, and select
//! items. Generic over translation direction (forward or reverse).

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    Expr, Join, JoinConstraint, JoinOperator, Query, SelectItem, TableFactor, TableWithJoins,
};

use crate::{errors::Error, prelude::Pg2SqliteOptions};

/// Abstracts the direction of translation so that shared helper functions
/// can work for both forward (`Translator`) and reverse (`ReverseTranslator`)
/// translation.
pub(crate) trait TranslationDirection {
    fn translate_expr(
        expr: &Expr,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Expr, Error>;
    fn translate_query(
        query: &Query,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Query, Error>;
}

pub(crate) fn translate_table_with_joins<D: TranslationDirection>(
    table_with_joins: &TableWithJoins,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableWithJoins, Error> {
    let translated_joins = table_with_joins
        .joins
        .iter()
        .map(|join| translate_join::<D>(join, schema, options))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(TableWithJoins {
        relation: translate_table_factor::<D>(&table_with_joins.relation, schema, options)?,
        joins: translated_joins,
    })
}

pub(crate) fn translate_join<D: TranslationDirection>(
    join: &Join,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Join, Error> {
    Ok(Join {
        relation: translate_table_factor::<D>(&join.relation, schema, options)?,
        global: join.global,
        join_operator: translate_join_operator::<D>(&join.join_operator, schema, options)?,
    })
}

pub(crate) fn translate_join_operator<D: TranslationDirection>(
    join_operator: &JoinOperator,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<JoinOperator, Error> {
    Ok(match join_operator {
        JoinOperator::Join(constraint) => {
            JoinOperator::Join(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::Inner(constraint) => {
            JoinOperator::Inner(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::Left(constraint) => {
            JoinOperator::Left(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::LeftOuter(constraint) => {
            JoinOperator::LeftOuter(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::Right(constraint) => {
            JoinOperator::Right(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::RightOuter(constraint) => {
            JoinOperator::RightOuter(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::FullOuter(constraint) => {
            JoinOperator::FullOuter(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::CrossJoin(constraint) => {
            JoinOperator::CrossJoin(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::Semi(constraint) => {
            JoinOperator::Semi(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::LeftSemi(constraint) => {
            JoinOperator::LeftSemi(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::RightSemi(constraint) => {
            JoinOperator::RightSemi(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::Anti(constraint) => {
            JoinOperator::Anti(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::LeftAnti(constraint) => {
            JoinOperator::LeftAnti(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::RightAnti(constraint) => {
            JoinOperator::RightAnti(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        JoinOperator::AsOf { constraint, match_condition } => {
            JoinOperator::AsOf {
                constraint: translate_join_constraint::<D>(constraint, schema, options)?,
                match_condition: D::translate_expr(match_condition, schema, options)?,
            }
        }
        JoinOperator::StraightJoin(constraint) => {
            JoinOperator::StraightJoin(translate_join_constraint::<D>(constraint, schema, options)?)
        }
        // These operators don't have constraints that need translation
        JoinOperator::CrossApply | JoinOperator::OuterApply => join_operator.clone(),
    })
}

pub(crate) fn translate_join_constraint<D: TranslationDirection>(
    constraint: &JoinConstraint,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<JoinConstraint, Error> {
    Ok(match constraint {
        JoinConstraint::On(expr) => JoinConstraint::On(D::translate_expr(expr, schema, options)?),
        // USING and other constraints don't need expression translation
        JoinConstraint::Using(idents) => JoinConstraint::Using(idents.clone()),
        JoinConstraint::Natural => JoinConstraint::Natural,
        JoinConstraint::None => JoinConstraint::None,
    })
}

pub(crate) fn translate_table_factor<D: TranslationDirection>(
    table_factor: &TableFactor,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableFactor, Error> {
    Ok(match table_factor {
        TableFactor::Derived { subquery, lateral, alias, sample } => {
            TableFactor::Derived {
                subquery: Box::new(D::translate_query(subquery, schema, options)?),
                lateral: *lateral,
                alias: alias.clone(),
                sample: sample.clone(),
            }
        }
        TableFactor::NestedJoin { table_with_joins, alias } => {
            TableFactor::NestedJoin {
                table_with_joins: Box::new(translate_table_with_joins::<D>(
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

pub(crate) fn translate_select_item<D: TranslationDirection>(
    item: &SelectItem,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<SelectItem, Error> {
    Ok(match item {
        SelectItem::UnnamedExpr(expr) => {
            SelectItem::UnnamedExpr(D::translate_expr(expr, schema, options)?)
        }
        SelectItem::ExprWithAlias { expr, alias } => {
            SelectItem::ExprWithAlias {
                expr: D::translate_expr(expr, schema, options)?,
                alias: alias.clone(),
            }
        }
        // Wildcards and qualified wildcards don't need translation
        other => other.clone(),
    })
}

pub(crate) fn translate_returning<D: TranslationDirection>(
    returning: Option<&Vec<SelectItem>>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Vec<SelectItem>>, Error> {
    returning
        .map(|items| {
            items
                .iter()
                .map(|item| translate_select_item::<D>(item, schema, options))
                .collect::<Result<Vec<_>, Error>>()
        })
        .transpose()
}
