//! Implementation of the [`ReverseTranslator`] trait for the
//! `Query`, `SetExpr`, and `Select` types.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Distinct, NamedWindowDefinition, Query, Select, SetExpr, Values};

use super::helpers::{
    reverse_translate_connect_by_kinds, reverse_translate_fetch_clause,
    reverse_translate_group_by_expr,
    reverse_translate_limit_clause as reverse_translate_limit_clause_shared,
    reverse_translate_named_windows, reverse_translate_order_by_clause,
    reverse_translate_order_by_expr, reverse_translate_pipe_operators,
    reverse_translate_query_settings, reverse_translate_select_item,
    reverse_translate_table_with_joins, reverse_translate_values_rows,
    reverse_translate_with_clause,
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
        let order_by = reverse_translate_order_by_clause(self.order_by.as_ref(), schema, options)?;
        let settings = reverse_translate_query_settings(self.settings.as_ref(), schema, options)?;
        let pipe_operators =
            reverse_translate_pipe_operators(&self.pipe_operators, schema, options)?;

        Ok(Query {
            with: reverse_translate_with_clause(self.with.as_ref(), schema, options)?,
            body: Box::new(self.body.reverse_translate(schema, options)?),
            order_by,
            limit_clause: reverse_translate_limit_clause_shared(
                self.limit_clause.as_ref(),
                schema,
                options,
            )?,
            fetch: reverse_translate_fetch_clause(self.fetch.as_ref(), schema, options)?,
            locks: self.locks.clone(),
            for_clause: self.for_clause.clone(),
            settings,
            format_clause: self.format_clause.clone(),
            pipe_operators,
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
        let prewhere = self
            .prewhere
            .as_ref()
            .map(|expr| expr.reverse_translate(schema, options))
            .transpose()?;
        let cluster_by = self
            .cluster_by
            .iter()
            .map(|expr| expr.reverse_translate(schema, options))
            .collect::<Result<Vec<_>, _>>()?;
        let distribute_by = self
            .distribute_by
            .iter()
            .map(|expr| expr.reverse_translate(schema, options))
            .collect::<Result<Vec<_>, _>>()?;
        let sort_by = self
            .sort_by
            .iter()
            .map(|expr| reverse_translate_order_by_expr(expr, schema, options))
            .collect::<Result<Vec<_>, _>>()?;
        let connect_by = reverse_translate_connect_by_kinds(&self.connect_by, schema, options)?;

        // Reverse translate GROUP BY expressions
        let group_by = reverse_translate_group_by(&self.group_by, schema, options)?;

        Ok(Select {
            select_token: self.select_token.clone(),
            distinct: reverse_translate_distinct(self.distinct.as_ref(), schema, options)?,
            top: self.top.clone(),
            top_before_distinct: self.top_before_distinct,
            projection,
            into: self.into.clone(),
            from,
            lateral_views: self.lateral_views.clone(),
            prewhere,
            selection,
            group_by,
            cluster_by,
            distribute_by,
            sort_by,
            having,
            named_window: reverse_translate_named_window(&self.named_window, schema, options)?,
            qualify: self
                .qualify
                .as_ref()
                .map(|e| e.reverse_translate(schema, options))
                .transpose()?,
            window_before_qualify: self.window_before_qualify,
            value_table_mode: self.value_table_mode,
            connect_by,
            flavor: self.flavor,
            exclude: self.exclude.clone(),
            optimizer_hint: self.optimizer_hint.clone(),
            select_modifiers: self.select_modifiers.clone(),
        })
    }
}

fn reverse_translate_values(
    values: &Values,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Values, Error> {
    reverse_translate_values_rows(values, schema, options)
}

#[cfg(test)]
fn reverse_translate_limit_clause(
    limit_clause: Option<&sqlparser::ast::LimitClause>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<sqlparser::ast::LimitClause>, Error> {
    reverse_translate_limit_clause_shared(limit_clause, schema, options)
}

#[cfg(test)]
fn reverse_translate_fetch(
    fetch: Option<&sqlparser::ast::Fetch>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<sqlparser::ast::Fetch>, Error> {
    reverse_translate_fetch_clause(fetch, schema, options)
}

fn reverse_translate_distinct(
    distinct: Option<&Distinct>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Distinct>, Error> {
    distinct
        .map(|d| {
            Ok(match d {
                Distinct::On(exprs) => {
                    let translated = exprs
                        .iter()
                        .map(|e| e.reverse_translate(schema, options))
                        .collect::<Result<Vec<_>, _>>()?;
                    Distinct::On(translated)
                }
                Distinct::Distinct => Distinct::Distinct,
                Distinct::All => Distinct::All,
            })
        })
        .transpose()
}

fn reverse_translate_named_window(
    named_windows: &[NamedWindowDefinition],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<NamedWindowDefinition>, Error> {
    reverse_translate_named_windows(named_windows, schema, options)
}

fn reverse_translate_group_by(
    group_by: &sqlparser::ast::GroupByExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::GroupByExpr, Error> {
    reverse_translate_group_by_expr(group_by, schema, options)
}

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{Distinct, LimitClause, Offset, Statement},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        reverse_translate_distinct, reverse_translate_fetch, reverse_translate_group_by,
        reverse_translate_limit_clause,
    };
    use crate::prelude::{Pg2SqliteOptions, ReverseTranslator};

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
            reverse_translate_limit_clause(Some(&limit_offset), &schema, &options).unwrap();
        assert!(matches!(translated, Some(LimitClause::LimitOffset { .. })));

        let offset_comma =
            LimitClause::OffsetCommaLimit { offset: parse_expr("1"), limit: parse_expr("10") };
        let translated =
            reverse_translate_limit_clause(Some(&offset_comma), &schema, &options).unwrap();
        assert!(matches!(translated, Some(LimitClause::OffsetCommaLimit { .. })));
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
            reverse_translate_distinct(select.distinct.as_ref(), &schema, &options).unwrap();
        assert!(matches!(distinct, Some(Distinct::On(_))));

        let group_by = reverse_translate_group_by(&select.group_by, &schema, &options).unwrap();
        assert!(matches!(group_by, sqlparser::ast::GroupByExpr::Expressions(_, _)));

        let fetch_query = parse_query("SELECT 1 FETCH FIRST 2 ROWS ONLY");
        let fetch = reverse_translate_fetch(fetch_query.fetch.as_ref(), &schema, &options).unwrap();
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
            options: sqlparser::ast::OrderByOptions { asc: Some(true), nulls_first: Some(false) },
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
                panic!("unexpected connect by variant: {:?}", &select.connect_by[0]);
            }
        }
        match &select.connect_by[1] {
            sqlparser::ast::ConnectByKind::StartWith { condition, .. } => {
                assert!(condition.to_string().to_lowercase().contains("now()"));
            }
            sqlparser::ast::ConnectByKind::ConnectBy { .. } => {
                panic!("unexpected connect by variant: {:?}", &select.connect_by[1]);
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
