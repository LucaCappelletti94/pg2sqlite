//! Implementation of the [`ReverseTranslator`] trait for the
//! `Query`, `SetExpr`, and `Select` types.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Query, Select, SetExpr};

use super::helpers::Reverse;
use crate::{
    errors::Error,
    impls::shared_helpers::{
        translate_query_shared, translate_select_shared, translate_set_expr_shared,
    },
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
        translate_query_shared::<Reverse>(self, schema, options)
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
        translate_set_expr_shared::<Reverse>(self, schema, options)
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
        translate_select_shared::<Reverse>(self, schema, options)
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{Distinct, LimitClause, Offset, Statement},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::super::helpers::Reverse;
    use crate::{
        impls::shared_helpers::{
            translate_distinct_shared, translate_fetch_clause, translate_limit_clause,
        },
        prelude::{Pg2SqliteOptions, ReverseTranslator},
    };

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap()
    }

    fn parse_query(sql: &str) -> sqlparser::ast::Query {
        let stmt = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap().remove(0);
        match stmt {
            Statement::Query(query) => *query,
            other => panic!("expected query, got: {other:?}"),
        }
    }

    fn parse_expr(expr: &str) -> sqlparser::ast::Expr {
        Parser::new(&PostgreSqlDialect {}).try_with_sql(expr).unwrap().parse_expr().unwrap()
    }

    #[test]
    fn reverse_translate_query_preserves_complex_clauses() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        let query = parse_query(
            r#"
            WITH c AS (SELECT 1 AS id)
            SELECT DISTINCT id,
                   SUM(id) OVER (PARTITION BY id ORDER BY id) AS s
            FROM c
            GROUP BY id
            ORDER BY id
            LIMIT 10 OFFSET 1
            FETCH FIRST 3 ROWS ONLY
            "#,
        );

        let translated = query.reverse_translate(&schema, &options).unwrap();
        assert!(translated.with.is_some());
        assert!(translated.order_by.is_some());
        assert!(translated.limit_clause.is_some());
        assert!(translated.fetch.is_some());
    }

    #[test]
    fn reverse_translate_limit_clause_covers_both_variants() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        let limit_offset = LimitClause::LimitOffset {
            limit: Some(parse_expr("10")),
            offset: Some(Offset { value: parse_expr("1"), rows: sqlparser::ast::OffsetRows::None }),
            limit_by: vec![parse_expr("2")],
        };
        let translated =
            translate_limit_clause::<Reverse>(Some(&limit_offset), &schema, &options).unwrap();
        assert!(matches!(translated, Some(LimitClause::LimitOffset { .. })));

        // The comma form has no PostgreSQL spelling, so it becomes LIMIT
        // OFFSET with the operands kept in place: offset 1, limit 10.
        let offset_comma =
            LimitClause::OffsetCommaLimit { offset: parse_expr("1"), limit: parse_expr("10") };
        let translated =
            translate_limit_clause::<Reverse>(Some(&offset_comma), &schema, &options).unwrap();
        let Some(LimitClause::LimitOffset { limit, offset, .. }) = translated else {
            panic!("expected the explicit form, got: {translated:?}");
        };
        assert_eq!(limit.map(|e| e.to_string()), Some("10".to_string()));
        assert_eq!(offset.map(|o| o.value.to_string()), Some("1".to_string()));
    }

    #[test]
    fn reverse_translate_fetch_distinct_and_group_by_cover_variants() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        let query = parse_query("SELECT DISTINCT ON (id) id FROM users GROUP BY id");
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected select");
        };

        let distinct =
            translate_distinct_shared::<Reverse>(select.distinct.as_ref(), &schema, &options)
                .unwrap();
        assert!(matches!(distinct, Some(Distinct::On(_))));

        let group_by = crate::impls::shared_helpers::translate_group_by_expr::<Reverse>(
            &select.group_by,
            &schema,
            &options,
        )
        .unwrap();
        assert!(matches!(group_by, sqlparser::ast::GroupByExpr::Expressions(_, _)));

        let fetch_query = parse_query("SELECT 1 FETCH FIRST 2 ROWS ONLY");
        let fetch =
            translate_fetch_clause::<Reverse>(fetch_query.fetch.as_ref(), &schema, &options)
                .unwrap();
        assert!(fetch.is_some());
    }

    #[test]
    fn reverse_translate_query_translates_select_side_and_query_level_expression_paths() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        let mut query = parse_query("SELECT id FROM users");
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_mut() else {
            panic!("expected select");
        };

        select.prewhere = Some(parse_expr("datetime('now')"));
        select.cluster_by = vec![parse_expr("datetime('now')")];
        select.distribute_by = vec![parse_expr("datetime('now')")];
        select.sort_by = vec![sqlparser::ast::OrderByExpr {
            expr: parse_expr("datetime('now')"),
            options: sqlparser::ast::OrderByOptions {
                sort: Some(sqlparser::ast::OrderBySort::Asc),
                nulls_first: Some(false),
            },
            with_fill: None,
        }];
        select.connect_by = vec![
            sqlparser::ast::ConnectByKind::ConnectBy {
                connect_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                nocycle: false,
                relationships: vec![parse_expr("datetime('now')")],
            },
            sqlparser::ast::ConnectByKind::StartWith {
                start_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                condition: Box::new(parse_expr("datetime('now')")),
            },
        ];

        query.settings = Some(vec![sqlparser::ast::Setting {
            key: sqlparser::ast::Ident::new("x"),
            value: parse_expr("datetime('now')"),
        }]);
        query.pipe_operators = vec![
            sqlparser::ast::PipeOperator::Where { expr: parse_expr("datetime('now')") },
            sqlparser::ast::PipeOperator::Union {
                set_quantifier: sqlparser::ast::SetQuantifier::All,
                queries: vec![parse_query("SELECT datetime('now') AS x")],
            },
        ];

        let translated = query.reverse_translate(&schema, &options).unwrap();
        let sqlparser::ast::SetExpr::Select(select) = translated.body.as_ref() else {
            panic!("expected translated select");
        };

        assert!(
            select
                .prewhere
                .as_ref()
                .is_some_and(|expr| expr.to_string().to_lowercase().contains("now()"))
        );
        assert!(select.cluster_by[0].to_string().to_lowercase().contains("now()"));
        assert!(select.distribute_by[0].to_string().to_lowercase().contains("now()"));
        assert!(select.sort_by[0].expr.to_string().to_lowercase().contains("now()"));

        match &select.connect_by[0] {
            sqlparser::ast::ConnectByKind::ConnectBy { relationships, .. } => {
                assert!(relationships[0].to_string().to_lowercase().contains("now()"));
            }
            sqlparser::ast::ConnectByKind::StartWith { .. } => {
                panic!("unexpected connect by variant: {:?}", select.connect_by[0]);
            }
        }
        match &select.connect_by[1] {
            sqlparser::ast::ConnectByKind::StartWith { condition, .. } => {
                assert!(condition.to_string().to_lowercase().contains("now()"));
            }
            sqlparser::ast::ConnectByKind::ConnectBy { .. } => {
                panic!("unexpected connect by variant: {:?}", select.connect_by[1]);
            }
        }

        assert!(translated.settings.as_ref().is_some_and(|settings| {
            settings[0].value.to_string().to_lowercase().contains("now()")
        }));

        match &translated.pipe_operators[0] {
            sqlparser::ast::PipeOperator::Where { expr } => {
                assert!(expr.to_string().to_lowercase().contains("now()"));
            }
            other => panic!("unexpected first pipe operator variant: {other:?}"),
        }
        match &translated.pipe_operators[1] {
            sqlparser::ast::PipeOperator::Union { queries, .. } => {
                assert!(queries[0].to_string().to_lowercase().contains("now()"));
            }
            other => panic!("unexpected second pipe operator variant: {other:?}"),
        }
    }
}
