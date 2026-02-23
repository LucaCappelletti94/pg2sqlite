//! Implementation of the [`ReverseTranslator`] trait for the
//! `Query`, `SetExpr`, and `Select` types.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    Distinct, Fetch, LimitClause, NamedWindowDefinition, NamedWindowExpr, Offset, Query, Select,
    SetExpr, Values,
};

use super::helpers::{reverse_translate_select_item, reverse_translate_table_with_joins};
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
            with: reverse_translate_with(self.with.as_ref(), schema, options)?,
            body: Box::new(self.body.reverse_translate(schema, options)?),
            order_by,
            limit_clause: reverse_translate_limit_clause(
                self.limit_clause.as_ref(),
                schema,
                options,
            )?,
            fetch: reverse_translate_fetch(self.fetch.as_ref(), schema, options)?,
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
            distinct: reverse_translate_distinct(self.distinct.as_ref(), schema, options)?,
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
            named_window: reverse_translate_named_window(&self.named_window, schema, options)?,
            qualify: self
                .qualify
                .as_ref()
                .map(|e| e.reverse_translate(schema, options))
                .transpose()?,
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

fn reverse_translate_with(
    with: Option<&sqlparser::ast::With>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<sqlparser::ast::With>, Error> {
    with.map(|w| {
        let cte_tables = w
            .cte_tables
            .iter()
            .map(|cte| {
                Ok(sqlparser::ast::Cte {
                    alias: cte.alias.clone(),
                    query: Box::new(cte.query.reverse_translate(schema, options)?),
                    from: cte.from.clone(),
                    materialized: cte.materialized,
                    closing_paren_token: cte.closing_paren_token.clone(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(sqlparser::ast::With {
            with_token: w.with_token.clone(),
            recursive: w.recursive,
            cte_tables,
        })
    })
    .transpose()
}

fn reverse_translate_limit_clause(
    limit_clause: Option<&LimitClause>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<LimitClause>, Error> {
    limit_clause
        .map(|lc| {
            Ok(match lc {
                LimitClause::LimitOffset { limit, offset, limit_by } => {
                    LimitClause::LimitOffset {
                        limit: limit
                            .as_ref()
                            .map(|e| e.reverse_translate(schema, options))
                            .transpose()?,
                        offset: offset
                            .as_ref()
                            .map(|o| {
                                Ok::<_, Error>(Offset {
                                    value: o.value.reverse_translate(schema, options)?,
                                    rows: o.rows,
                                })
                            })
                            .transpose()?,
                        limit_by: limit_by
                            .iter()
                            .map(|e| e.reverse_translate(schema, options))
                            .collect::<Result<Vec<_>, _>>()?,
                    }
                }
                LimitClause::OffsetCommaLimit { offset, limit } => {
                    LimitClause::OffsetCommaLimit {
                        offset: offset.reverse_translate(schema, options)?,
                        limit: limit.reverse_translate(schema, options)?,
                    }
                }
            })
        })
        .transpose()
}

fn reverse_translate_fetch(
    fetch: Option<&Fetch>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Fetch>, Error> {
    fetch
        .map(|f| {
            Ok(Fetch {
                with_ties: f.with_ties,
                percent: f.percent,
                quantity: f
                    .quantity
                    .as_ref()
                    .map(|e| e.reverse_translate(schema, options))
                    .transpose()?,
            })
        })
        .transpose()
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
    named_windows
        .iter()
        .map(|nwd| {
            let translated_expr = match &nwd.1 {
                NamedWindowExpr::NamedWindow(ident) => NamedWindowExpr::NamedWindow(ident.clone()),
                NamedWindowExpr::WindowSpec(spec) => {
                    NamedWindowExpr::WindowSpec(reverse_translate_window_spec(
                        spec, schema, options,
                    )?)
                }
            };
            Ok(NamedWindowDefinition(nwd.0.clone(), translated_expr))
        })
        .collect()
}

fn reverse_translate_window_spec(
    spec: &sqlparser::ast::WindowSpec,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::WindowSpec, Error> {
    let partition_by = spec
        .partition_by
        .iter()
        .map(|e| e.reverse_translate(schema, options))
        .collect::<Result<Vec<_>, _>>()?;
    let order_by = spec
        .order_by
        .iter()
        .map(|e| reverse_translate_order_by_expr(e, schema, options))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(sqlparser::ast::WindowSpec {
        window_name: spec.window_name.clone(),
        partition_by,
        order_by,
        window_frame: spec.window_frame.clone(),
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
}
