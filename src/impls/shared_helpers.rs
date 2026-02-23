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

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{
            Expr, JoinConstraint, JoinOperator, Query, SelectItem, Statement, TableFactor,
            ValueWithSpan,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        TranslationDirection, translate_join, translate_join_constraint, translate_join_operator,
        translate_returning, translate_select_item, translate_table_factor,
        translate_table_with_joins,
    };
    use crate::{errors::Error, prelude::Pg2SqliteOptions};

    struct IdentityDirection;

    impl TranslationDirection for IdentityDirection {
        fn translate_expr(
            expr: &Expr,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
        ) -> Result<Expr, Error> {
            Ok(expr.clone())
        }

        fn translate_query(
            query: &Query,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
        ) -> Result<Query, Error> {
            Ok(query.clone())
        }
    }

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap()
    }

    fn parse_query(sql: &str) -> Query {
        let stmts = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap();
        match stmts.into_iter().next().unwrap() {
            Statement::Query(query) => *query,
            other => panic!("expected query statement, got: {other:?}"),
        }
    }

    #[test]
    fn translates_join_structures_and_select_items() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let query = parse_query(
            "SELECT t.a AS a1 FROM t INNER JOIN u ON t.id = u.id LEFT JOIN v ON u.id = v.uid",
        );
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected select");
        };

        let translated = translate_table_with_joins::<IdentityDirection>(
            select.from.first().unwrap(),
            &schema,
            &options,
        )
        .unwrap();
        assert_eq!(translated.joins.len(), 2);

        let unnamed = SelectItem::UnnamedExpr(Expr::Identifier(sqlparser::ast::Ident::new("a")));
        let named = SelectItem::ExprWithAlias {
            expr: Expr::Identifier(sqlparser::ast::Ident::new("b")),
            alias: sqlparser::ast::Ident::new("b1"),
        };
        assert!(matches!(
            translate_select_item::<IdentityDirection>(&unnamed, &schema, &options).unwrap(),
            SelectItem::UnnamedExpr(_)
        ));
        assert!(matches!(
            translate_select_item::<IdentityDirection>(&named, &schema, &options).unwrap(),
            SelectItem::ExprWithAlias { .. }
        ));
    }

    #[test]
    fn translates_all_join_operator_variants() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let on = JoinConstraint::On(Expr::Value(ValueWithSpan::from(
            sqlparser::ast::Value::Boolean(true),
        )));

        let operators = vec![
            JoinOperator::Join(on.clone()),
            JoinOperator::Inner(on.clone()),
            JoinOperator::Left(on.clone()),
            JoinOperator::LeftOuter(on.clone()),
            JoinOperator::Right(on.clone()),
            JoinOperator::RightOuter(on.clone()),
            JoinOperator::FullOuter(on.clone()),
            JoinOperator::CrossJoin(on.clone()),
            JoinOperator::Semi(on.clone()),
            JoinOperator::LeftSemi(on.clone()),
            JoinOperator::RightSemi(on.clone()),
            JoinOperator::Anti(on.clone()),
            JoinOperator::LeftAnti(on.clone()),
            JoinOperator::RightAnti(on.clone()),
            JoinOperator::AsOf {
                constraint: on.clone(),
                match_condition: Expr::Value(ValueWithSpan::from(sqlparser::ast::Value::Number(
                    "1".to_string(),
                    false,
                ))),
            },
            JoinOperator::StraightJoin(on.clone()),
            JoinOperator::CrossApply,
            JoinOperator::OuterApply,
        ];

        for op in &operators {
            let _ = translate_join_operator::<IdentityDirection>(op, &schema, &options).unwrap();
        }

        let _ = translate_join_constraint::<IdentityDirection>(&on, &schema, &options).unwrap();
    }

    #[test]
    fn translates_table_factor_and_returning() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let query = parse_query("SELECT * FROM (SELECT 1) AS q");
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected select");
        };

        let derived = &select.from[0].relation;
        let _ = translate_table_factor::<IdentityDirection>(derived, &schema, &options).unwrap();

        let nested_query = parse_query("SELECT * FROM (t JOIN u ON t.id = u.id) AS z");
        let sqlparser::ast::SetExpr::Select(nested_select) = nested_query.body.as_ref() else {
            panic!("expected select");
        };
        let nested_factor = &nested_select.from[0].relation;
        if let TableFactor::NestedJoin { .. } = nested_factor {
            let _ = translate_table_factor::<IdentityDirection>(nested_factor, &schema, &options)
                .unwrap();
        }

        let returning_items = vec![
            SelectItem::UnnamedExpr(Expr::Identifier(sqlparser::ast::Ident::new("id"))),
            SelectItem::ExprWithAlias {
                expr: Expr::Identifier(sqlparser::ast::Ident::new("name")),
                alias: sqlparser::ast::Ident::new("n"),
            },
        ];
        assert_eq!(
            translate_returning::<IdentityDirection>(Some(&returning_items), &schema, &options)
                .unwrap()
                .unwrap()
                .len(),
            2
        );
        assert!(
            translate_returning::<IdentityDirection>(None, &schema, &options).unwrap().is_none()
        );
    }

    #[test]
    fn translate_join_preserves_global_flag() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let query = parse_query("SELECT * FROM t INNER JOIN u ON t.id = u.id");
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected select");
        };
        let mut join = select.from[0].joins[0].clone();
        join.global = true;
        let translated = translate_join::<IdentityDirection>(&join, &schema, &options).unwrap();
        assert!(translated.global);
    }
}
