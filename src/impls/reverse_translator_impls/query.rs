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
use sqlparser::ast::{
    Distinct, Expr, OrderBy, OrderByExpr, OrderByKind, Query, Select, SelectItem, SetExpr,
    TableFactor, TableWithJoins, WindowType,
};

use super::helpers::Reverse;
use crate::{
    errors::Error,
    impls::{
        shared_helpers::{
            translate_query_shared, translate_select_shared, translate_set_expr_shared,
        },
        translator_impls::query::{DISTINCT_ON_ROWNUM_ALIAS, distinct_on_window_select},
    },
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

/// The inner select, its projected aliases, and the window partition and order
/// of a `ROW_NUMBER()` filter carrying the forward direction's row number
/// marker. The inner select is returned without the row number column.
fn distinct_on_window_parts(outer: &Select) -> Option<(Select, Vec<Expr>, Vec<OrderByExpr>)> {
    let [TableWithJoins { relation: TableFactor::Derived { subquery, .. }, .. }] =
        outer.from.as_slice()
    else {
        return None;
    };
    let SetExpr::Select(inner) = subquery.body.as_ref() else {
        return None;
    };
    if inner.distinct.is_some() {
        return None;
    }

    let (row_number, projected) = inner.projection.split_last()?;
    let SelectItem::ExprWithAlias { expr: Expr::Function(row_number), alias } = row_number else {
        return None;
    };
    if alias.value != DISTINCT_ON_ROWNUM_ALIAS {
        return None;
    }
    let Some(WindowType::WindowSpec(window)) = row_number.over.as_ref() else {
        return None;
    };

    let aliases = projected
        .iter()
        .map(|item| {
            match item {
                SelectItem::ExprWithAlias { alias, .. } => Some(alias.clone()),
                _ => None,
            }
        })
        .collect::<Option<Vec<_>>>()?;

    let mut stripped = inner.as_ref().clone();
    stripped.projection = projected.to_vec();

    // Rebuilding through the emitter and comparing is what proves nothing else
    // rides on the outer select, so nothing can be dropped here unnoticed.
    let rebuilt = distinct_on_window_select(
        stripped.clone(),
        &aliases,
        window.partition_by.clone(),
        window.order_by.clone(),
    );
    if rebuilt != *outer {
        return None;
    }

    Some((stripped, window.partition_by.clone(), window.order_by.clone()))
}

/// The `ORDER BY` expressions of `query`, or `None` when it carries an ordering
/// the forward `DISTINCT ON` rewrite never produces.
fn plain_order_by(query: &Query) -> Option<&[OrderByExpr]> {
    match query.order_by.as_ref() {
        None => Some(&[]),
        Some(OrderBy { kind: OrderByKind::Expressions(exprs), interpolate: None }) => Some(exprs),
        Some(_) => None,
    }
}

/// Drops an alias that only repeats the name of the column it labels. The
/// forward rewrite adds those to name every projected column.
fn strip_redundant_alias(item: SelectItem) -> SelectItem {
    match item {
        SelectItem::ExprWithAlias { expr: Expr::Identifier(ident), alias } if ident == alias => {
            SelectItem::UnnamedExpr(Expr::Identifier(ident))
        }
        SelectItem::ExprWithAlias { expr: Expr::CompoundIdentifier(parts), alias }
            if parts.last() == Some(&alias) =>
        {
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts))
        }
        other => other,
    }
}

/// Rebuilds the `DISTINCT ON` query that the forward direction rewrote into a
/// `ROW_NUMBER()` filter over a derived table.
///
/// The two orderings are NOT the same list, and reading them as one is what let
/// the forward direction emit SQL that would not prepare. The window keeps
/// every operand, because it decides which row per partition survives, and it
/// sits inside the derived table where the source columns are in scope. The
/// outer `ORDER BY` keeps only the leading `DISTINCT ON` operands, because that
/// is all the derived table exposes and because those already order the
/// deduplicated result totally, so the tail is unobservable there.
///
/// So the outer ordering must be the window ordering's prefix of exactly the
/// partition length, and the restored PostgreSQL takes the FULL window
/// ordering, which is the list PostgreSQL needs to pick the same row.
/// PostgreSQL also rejects a `DISTINCT ON` whose expressions are not the
/// initial `ORDER BY` expressions, so a shape breaching that is left as it
/// stands.
fn restore_distinct_on(query: &Query) -> Option<Query> {
    let SetExpr::Select(outer) = query.body.as_ref() else {
        return None;
    };
    let (inner, partition_by, window_order) = distinct_on_window_parts(outer)?;

    let query_order = plain_order_by(query)?;
    let expected_outer = &window_order[..window_order.len().min(partition_by.len())];
    if query_order != expected_outer {
        return None;
    }
    if !window_order.is_empty()
        && (window_order.len() < partition_by.len()
            || !partition_by.iter().zip(&window_order).all(|(on, order)| *on == order.expr))
    {
        return None;
    }

    let mut select = inner;
    select.projection = select.projection.into_iter().map(strip_redundant_alias).collect();
    select.distinct = Some(Distinct::On(partition_by));

    let restored_order = (!window_order.is_empty())
        .then_some(OrderBy { kind: OrderByKind::Expressions(window_order), interpolate: None });
    Some(Query {
        body: Box::new(SetExpr::Select(Box::new(select))),
        order_by: restored_order,
        ..query.clone()
    })
}

impl ReverseTranslator for Query {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Query;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        let restored = restore_distinct_on(self);
        translate_query_shared::<Reverse>(restored.as_ref().unwrap_or(self), schema, options)
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
