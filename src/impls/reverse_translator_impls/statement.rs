//! Implementation of the [`ReverseTranslator`] trait for the
//! `Statement` type.

use core::ops::ControlFlow;
use std::collections::HashSet;

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    Delete, Expr, Insert, ObjectName, Query, SetExpr, Statement, Table, TableObject, Update, Visit,
    Visitor,
};
#[cfg(test)]
use sqlparser::ast::{LimitClause, TableFactor};

use crate::{
    errors::Error,
    impls::{object_name::last_ident, shared_helpers::debug_variant_name},
    prelude::{Pg2SqliteOptions, ReverseTranslator},
    traits::TranslationOptions,
};

/// Check if a table name ends with the RLS suffix.
fn strip_identifier_quotes(name: &str) -> &str {
    if name.len() >= 2 {
        let first = name.as_bytes()[0] as char;
        let last = name.as_bytes()[name.len() - 1] as char;
        if matches!((first, last), ('"', '"') | ('`', '`') | ('[', ']')) {
            return &name[1..name.len() - 1];
        }
    }
    name
}

/// Checks whether an identifier string ends with a suffix, ignoring outer
/// quotes.
fn identifier_has_suffix(name: &str, suffix: &str) -> bool {
    strip_identifier_quotes(name).ends_with(suffix)
}

/// Check if a table name ends with the RLS suffix.
fn is_rls_table(name: &ObjectName, options: &Pg2SqliteOptions) -> bool {
    let suffix = options.get_rls_table_suffix();
    last_ident(name).is_some_and(|ident| ident.value.ends_with(suffix))
}

/// Check a table reference for RLS table access.
fn check_table_for_rls(name: &ObjectName, options: &Pg2SqliteOptions) -> Result<(), Error> {
    if is_rls_table(name, options) {
        return Err(Error::RlsTableDetected {
            table_name: name.to_string(),
            suffix: options.get_rls_table_suffix().to_string(),
        });
    }
    Ok(())
}

/// Check a TableObject for RLS table access.
fn check_table_object_for_rls(
    table: &TableObject,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match table {
        TableObject::TableName(name) => check_table_for_rls(name, options),
        TableObject::TableFunction(_) => Ok(()),
    }
}

fn check_table_command_for_rls(table: &Table, options: &Pg2SqliteOptions) -> Result<(), Error> {
    if let Some(table_name) = &table.table_name {
        let full_name = table
            .schema_name
            .as_ref()
            .map_or_else(|| table_name.clone(), |schema| format!("{schema}.{table_name}"));
        let suffix = options.get_rls_table_suffix();
        if identifier_has_suffix(table_name, suffix) {
            return Err(Error::RlsTableDetected {
                table_name: full_name,
                suffix: suffix.to_string(),
            });
        }
    }
    Ok(())
}

fn table_command_name(table: &Table) -> Option<&str> {
    table.table_name.as_deref()
}

fn ensure_set_expr_supported_for_rls(
    set_expr: &SetExpr,
    options: &Pg2SqliteOptions,
    cte_scopes: &[HashSet<String>],
) -> Result<(), Error> {
    match set_expr {
        SetExpr::SetOperation { left, right, .. } => {
            ensure_set_expr_supported_for_rls(left, options, cte_scopes)?;
            ensure_set_expr_supported_for_rls(right, options, cte_scopes)
        }
        SetExpr::Table(table) => {
            if let Some(table_name) = table_command_name(table) {
                let normalized = strip_identifier_quotes(table_name).to_ascii_lowercase();
                if cte_scopes.iter().rev().any(|scope| scope.contains(&normalized)) {
                    return Ok(());
                }
            }
            check_table_command_for_rls(table, options)
        }
        SetExpr::Merge(_) => {
            Err(Error::UnsupportedSQLiteFeature(
                "MERGE set expressions are not supported in reverse translation".to_string(),
            ))
        }
        SetExpr::Query(_)
        | SetExpr::Select(_)
        | SetExpr::Insert(_)
        | SetExpr::Update(_)
        | SetExpr::Delete(_)
        | SetExpr::Values(_) => Ok(()),
    }
}

struct RlsAstVisitor<'a> {
    options: &'a Pg2SqliteOptions,
    cte_scopes: Vec<HashSet<String>>,
}

impl RlsAstVisitor<'_> {
    fn is_cte_relation(&self, relation: &ObjectName) -> bool {
        let Some(last) = last_ident(relation) else {
            return false;
        };
        let normalized = strip_identifier_quotes(&last.value).to_ascii_lowercase();
        self.cte_scopes.iter().rev().any(|scope| scope.contains(&normalized))
    }
}

impl Visitor for RlsAstVisitor<'_> {
    type Break = Box<Error>;

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        let current_scope = query
            .with
            .as_ref()
            .map(|with| {
                with.cte_tables
                    .iter()
                    .map(|cte| cte.alias.name.value.to_ascii_lowercase())
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        self.cte_scopes.push(current_scope);

        match ensure_set_expr_supported_for_rls(query.body.as_ref(), self.options, &self.cte_scopes)
        {
            Ok(()) => ControlFlow::Continue(()),
            Err(err) => {
                let _ = self.cte_scopes.pop();
                ControlFlow::Break(Box::new(err))
            }
        }
    }

    fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
        let _ = self.cte_scopes.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<Self::Break> {
        if self.is_cte_relation(relation) {
            return ControlFlow::Continue(());
        }
        match check_table_for_rls(relation, self.options) {
            Ok(()) => ControlFlow::Continue(()),
            Err(err) => ControlFlow::Break(Box::new(err)),
        }
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        if let Expr::MatchAgainst { .. } = expr {
            return ControlFlow::Break(Box::new(Error::UnsupportedRlsExpressionVariant {
                expr_variant: debug_variant_name(expr),
                expression: expr.to_string(),
            }));
        }
        ControlFlow::Continue(())
    }
}

fn run_rls_visitor<T: Visit>(node: &T, options: &Pg2SqliteOptions) -> Result<(), Error> {
    let mut visitor = RlsAstVisitor { options, cte_scopes: Vec::new() };
    match node.visit(&mut visitor) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(err) => Err(*err),
    }
}

#[cfg(test)]
fn check_table_factor_for_rls(
    factor: &TableFactor,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    run_rls_visitor(factor, options)
}

/// Check an expression tree for RLS table references in subqueries.
#[cfg(test)]
fn check_expr_for_rls(expr: &Expr, options: &Pg2SqliteOptions) -> Result<(), Error> {
    run_rls_visitor(expr, options)
}

#[cfg(test)]
fn check_set_expr_for_rls(set_expr: &SetExpr, options: &Pg2SqliteOptions) -> Result<(), Error> {
    ensure_set_expr_supported_for_rls(set_expr, options, &[])?;
    run_rls_visitor(set_expr, options)
}

#[cfg(test)]
fn check_limit_clause_for_rls(
    limit_clause: &LimitClause,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    run_rls_visitor(limit_clause, options)
}

fn check_query_for_rls(query: &Query, options: &Pg2SqliteOptions) -> Result<(), Error> {
    run_rls_visitor(query, options)
}
fn check_insert_for_rls(insert: &Insert, options: &Pg2SqliteOptions) -> Result<(), Error> {
    check_table_object_for_rls(&insert.table, options)?;
    run_rls_visitor(insert, options)
}

fn check_update_for_rls(update: &Update, options: &Pg2SqliteOptions) -> Result<(), Error> {
    run_rls_visitor(update, options)
}

fn check_delete_for_rls(delete: &Delete, options: &Pg2SqliteOptions) -> Result<(), Error> {
    run_rls_visitor(delete, options)
}

impl ReverseTranslator for Statement {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Statement;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        match self {
            Statement::Insert(insert) => {
                check_insert_for_rls(insert, options)?;

                Ok(Statement::Insert(insert.reverse_translate(schema, options)?))
            }
            Statement::Update(update) => {
                check_update_for_rls(update, options)?;

                Ok(Statement::Update(update.reverse_translate(schema, options)?))
            }
            Statement::Delete(delete) => {
                check_delete_for_rls(delete, options)?;

                Ok(Statement::Delete(delete.reverse_translate(schema, options)?))
            }
            Statement::Query(query) => {
                check_query_for_rls(query, options)?;

                Ok(Statement::Query(Box::new(query.reverse_translate(schema, options)?)))
            }
            // Transaction control statements pass through unchanged
            Statement::Commit { .. }
            | Statement::Rollback { .. }
            | Statement::StartTransaction { .. }
            | Statement::Savepoint { .. }
            | Statement::ReleaseSavepoint { .. } => Ok(self.clone()),
            // Non-DML statements are not supported for reverse translation
            other => {
                let variant_name = debug_variant_name(other);
                Err(Error::UnsupportedReverseStatement { statement_type: variant_name })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{
            AccessExpr, Expr, LimitClause, Offset, Query, SetExpr, Statement, Subscript,
            TableFactor,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        check_expr_for_rls, check_limit_clause_for_rls, check_query_for_rls,
        check_set_expr_for_rls, check_table_factor_for_rls,
    };
    use crate::{
        errors::Error,
        prelude::{Pg2SqliteOptions, ReverseTranslator},
    };

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap()
    }

    fn parse_expr(expr: &str) -> Expr {
        Parser::new(&PostgreSqlDialect {}).try_with_sql(expr).unwrap().parse_expr().unwrap()
    }

    fn parse_query(sql: &str) -> Query {
        let stmt = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap().remove(0);
        match stmt {
            Statement::Query(query) => *query,
            other => panic!("expected query, got: {other:?}"),
        }
    }

    #[test]
    fn check_expr_for_rls_accepts_many_expression_variants() {
        let options = Pg2SqliteOptions::default();
        let expressions = vec![
            "a = ANY(b)",
            "a = ALL(b)",
            "CASE WHEN x > 0 THEN y ELSE z END",
            "TRIM(BOTH 'x' FROM col)",
            "SUBSTRING(col FROM 1 FOR 2)",
            "OVERLAY(col PLACING 'x' FROM 1 FOR 1)",
            "POSITION('x' IN col)",
            "a AT TIME ZONE 'UTC'",
            "a LIKE b",
            "a ILIKE b",
            "a SIMILAR TO b",
            "a RLIKE b",
            "ARRAY[1,2]",
            "(SELECT 1)",
            "EXISTS (SELECT 1)",
            "x IN (SELECT 1)",
            "(1, 2)",
            "INTERVAL '1 day'",
            "'abc' COLLATE \"C\"",
            "foo[0]",
        ];

        for raw in expressions {
            let expr = parse_expr(raw);
            check_expr_for_rls(&expr, &options).unwrap();
        }
    }

    #[test]
    fn check_expr_for_rls_rejects_subquery_in_subscript_index() {
        let options = Pg2SqliteOptions::default();
        let expr = Expr::CompoundFieldAccess {
            root: Box::new(parse_expr("payload")),
            access_chain: vec![AccessExpr::Subscript(Subscript::Index {
                index: Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1"))),
            })],
        };

        let err = check_expr_for_rls(&expr, &options).unwrap_err();
        assert!(err.to_string().contains("users_rls"));
    }

    #[test]
    fn check_query_for_rls_covers_with_order_by_limit_fetch_and_function_shapes() {
        let options = Pg2SqliteOptions::default();
        let query = parse_query(
            r#"
            WITH c AS (SELECT 1 AS id)
            SELECT
                id,
                percentile_disc(0.5) WITHIN GROUP (ORDER BY id),
                sum(id) FILTER (WHERE id > 0) OVER (PARTITION BY id ORDER BY id)
            FROM c
            WHERE id IN (SELECT id FROM c)
            GROUP BY id
            HAVING id > 0
            ORDER BY id
            LIMIT 10 OFFSET 1
            FETCH FIRST 5 ROWS ONLY
            "#,
        );

        check_query_for_rls(&query, &options).unwrap();
    }

    #[test]
    fn check_set_expr_for_rls_handles_insert_update_delete_values_and_table_variants() {
        let options = Pg2SqliteOptions::default();

        let insert_stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, "INSERT INTO users(id) VALUES (1)")
                .unwrap()
                .remove(0);
        if let Statement::Insert(insert) = insert_stmt {
            check_set_expr_for_rls(&SetExpr::Insert(Statement::Insert(insert)), &options).unwrap();
        } else {
            panic!("expected insert");
        }

        let update_stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, "UPDATE users SET id = 1").unwrap().remove(0);
        if let Statement::Update(update) = update_stmt {
            check_set_expr_for_rls(&SetExpr::Update(Statement::Update(update)), &options).unwrap();
        } else {
            panic!("expected update");
        }

        let delete_stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, "DELETE FROM users WHERE id = 1")
                .unwrap()
                .remove(0);
        if let Statement::Delete(delete) = delete_stmt {
            check_set_expr_for_rls(&SetExpr::Delete(Statement::Delete(delete)), &options).unwrap();
        } else {
            panic!("expected delete");
        }

        let values_query = parse_query("VALUES (1), (2)");
        check_set_expr_for_rls(values_query.body.as_ref(), &options).unwrap();

        let table_expr = SetExpr::Table(Box::new(sqlparser::ast::Table {
            table_name: Some("users".to_string()),
            schema_name: None,
        }));
        check_set_expr_for_rls(&table_expr, &options).unwrap();

        let rls_table_expr = SetExpr::Table(Box::new(sqlparser::ast::Table {
            table_name: Some("users_rls".to_string()),
            schema_name: None,
        }));
        let err = check_set_expr_for_rls(&rls_table_expr, &options).unwrap_err();
        assert!(err.to_string().contains("users_rls"));

        let merge_stmt = Parser::parse_sql(&PostgreSqlDialect {}, "COMMIT;").unwrap().remove(0);
        let merge_expr = SetExpr::Merge(merge_stmt);
        let err = check_set_expr_for_rls(&merge_expr, &options)
            .expect_err("merge set expr should be rejected");
        assert!(err.to_string().contains("MERGE set expressions"));
    }

    #[test]
    fn check_limit_clause_for_rls_handles_offset_comma_limit_variant() {
        let options = Pg2SqliteOptions::default();
        let offset_comma =
            LimitClause::OffsetCommaLimit { offset: parse_expr("1"), limit: parse_expr("10") };
        check_limit_clause_for_rls(&offset_comma, &options).unwrap();

        let limit_offset = LimitClause::LimitOffset {
            limit: Some(parse_expr("10")),
            offset: Some(Offset { value: parse_expr("1"), rows: sqlparser::ast::OffsetRows::None }),
            limit_by: vec![parse_expr("2")],
        };
        check_limit_clause_for_rls(&limit_offset, &options).unwrap();
    }

    #[test]
    fn reverse_translate_rejects_rls_backing_tables_and_non_dml_statements() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        let query_stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, "SELECT * FROM users_rls").unwrap().remove(0);
        let err = query_stmt.reverse_translate(&schema, &options).unwrap_err();
        assert!(err.to_string().contains("Direct access to RLS backing table"));

        let non_dml = Parser::parse_sql(&PostgreSqlDialect {}, "VACUUM").unwrap().remove(0);
        let err = non_dml.reverse_translate(&schema, &options).unwrap_err();
        assert!(err.to_string().contains("Reverse translation only supports DML statements"));
    }

    #[test]
    fn check_table_factor_and_set_expr_cover_query_fallback_paths() {
        let options = Pg2SqliteOptions::default();

        let table_fn_query = parse_query("SELECT * FROM generate_series(1, 2)");
        let sqlparser::ast::SetExpr::Select(select) = table_fn_query.body.as_ref() else {
            panic!("expected select");
        };
        check_table_factor_for_rls(&select.from[0].relation, &options).unwrap();

        let manual_table_function =
            TableFactor::TableFunction { expr: parse_expr("generate_series(1, 2)"), alias: None };
        check_table_factor_for_rls(&manual_table_function, &options).unwrap();

        let set_expr = SetExpr::Query(Box::new(parse_query("SELECT 1")));
        check_set_expr_for_rls(&set_expr, &options).unwrap();
    }

    #[test]
    fn check_expr_for_rls_rejects_grouping_sets_with_rls_subquery() {
        let options = Pg2SqliteOptions::default();
        let expr = Expr::GroupingSets(vec![vec![Expr::Subquery(Box::new(parse_query(
            "SELECT id FROM users_rls LIMIT 1",
        )))]]);

        let err = check_expr_for_rls(&expr, &options)
            .expect_err("GROUPING SETS expressions with RLS-backed subqueries should be rejected");
        assert!(err.to_string().contains("users_rls"));
    }

    #[test]
    fn check_query_for_rls_rejects_select_side_paths_with_rls_subqueries() {
        let options = Pg2SqliteOptions::default();
        let mut query = parse_query("SELECT id FROM users");
        let SetExpr::Select(select) = query.body.as_mut() else {
            panic!("expected select");
        };

        select.prewhere =
            Some(Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1"))));
        select.cluster_by =
            vec![Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1")))];
        select.distribute_by =
            vec![Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1")))];
        select.sort_by = vec![sqlparser::ast::OrderByExpr {
            expr: Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1"))),
            options: sqlparser::ast::OrderByOptions { asc: None, nulls_first: None },
            with_fill: None,
        }];

        let err = check_query_for_rls(&query, &options).expect_err(
            "Select-side expression vectors should be traversed for RLS-backed subqueries",
        );
        assert!(err.to_string().contains("users_rls"));
    }

    #[test]
    fn check_query_for_rls_rejects_query_settings_and_pipe_exprs() {
        let options = Pg2SqliteOptions::default();
        let mut query = parse_query("SELECT id FROM users");
        query.settings = Some(vec![sqlparser::ast::Setting {
            key: sqlparser::ast::Ident::new("x"),
            value: Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1"))),
        }]);
        query.pipe_operators = vec![sqlparser::ast::PipeOperator::Where {
            expr: Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1"))),
        }];

        let err = check_query_for_rls(&query, &options).expect_err(
            "Query settings and pipe operators should be traversed for RLS-backed subqueries",
        );
        assert!(err.to_string().contains("users_rls"));
    }

    #[test]
    fn check_expr_for_rls_rejects_function_subquery_arguments() {
        let options = Pg2SqliteOptions::default();
        let expr = Expr::Function(sqlparser::ast::Function {
            name: sqlparser::ast::ObjectName::from(vec![sqlparser::ast::Ident::new("array")]),
            uses_odbc_syntax: false,
            parameters: sqlparser::ast::FunctionArguments::None,
            args: sqlparser::ast::FunctionArguments::Subquery(Box::new(parse_query(
                "SELECT id FROM users_rls LIMIT 1",
            ))),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: Vec::new(),
        });

        let err = check_expr_for_rls(&expr, &options)
            .expect_err("Function subquery arguments should be checked for RLS-backed tables");
        assert!(err.to_string().contains("users_rls"));
    }

    #[test]
    fn check_expr_for_rls_rejects_unhandled_match_against_variant() {
        let options = Pg2SqliteOptions::default();
        let expr = Expr::MatchAgainst {
            columns: vec![sqlparser::ast::ObjectName::from(vec![sqlparser::ast::Ident::new(
                "body",
            )])],
            match_value: sqlparser::ast::Value::SingleQuotedString("term".to_string()),
            opt_search_modifier: None,
        };

        let err = check_expr_for_rls(&expr, &options)
            .expect_err("unhandled expression variants must fail closed in RLS checks");
        assert!(matches!(err, Error::UnsupportedRlsExpressionVariant { .. }));
        assert!(err.to_string().contains("MatchAgainst"));
    }
}
