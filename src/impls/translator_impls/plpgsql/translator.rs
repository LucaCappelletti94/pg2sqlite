//! Main translator for PL/pgSQL to `SQLite`.
//!
//! This module orchestrates the translation of PL/pgSQL function bodies
//! to SQLite-compatible statements.

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
use sqlparser::{
    ast::{
        BeginEndStatements, BinaryOperator, Expr, FunctionArg, FunctionArgExpr, FunctionArguments,
        GroupByExpr, Ident, ObjectName, ObjectNamePart, Query, Select, SelectItem, Set, SetExpr,
        Statement, TableAlias, TableFactor, TableWithJoins, Value, ValueWithSpan,
    },
    tokenizer::Span,
};

use super::{
    context::{PlPgSqlContext, VariableBinding},
    cte_builder::{CteBuilder, make_query, make_simple_select},
};
use crate::{
    errors::Error,
    impls::{
        expr_helpers::{any_child_expr, map_expr_children},
        translator_impls::condition_injection::inject_condition_into_dml_statement,
    },
    options::Pg2SqliteOptions,
    traits::{TranslationOptions, Translator},
};

/// Main translator for PL/pgSQL function bodies.
pub struct PlPgSqlTranslator;

impl PlPgSqlTranslator {
    /// Translates a PL/pgSQL function body to `SQLite` statements.
    ///
    /// This is the main entry point for translation. It:
    /// 1. Preprocesses the body to handle PL/pgSQL-specific syntax
    /// 2. Builds a context with variable declarations
    /// 3. Translates each statement with proper CTE injection
    ///
    /// # Arguments
    /// * `body` - The parsed BEGIN...END block from the PL/pgSQL function
    /// * `schema` - The database schema for resolving references
    /// * `options` - Translation options
    ///
    /// # Returns
    /// A vector of SQLite-compatible statements
    ///
    /// # Errors
    /// Returns an error if translation fails for any statement.
    pub fn translate(
        body: &BeginEndStatements,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, Error> {
        Self::translate_with_context(body, PlPgSqlContext::new(), schema, options)
    }

    /// Translates a PL/pgSQL function body using a pre-seeded context.
    ///
    /// This is used when DECLARE defaults were parsed before statement parsing
    /// and need to be available during translation.
    ///
    /// # Errors
    /// Returns an error if translation fails for any statement.
    pub fn translate_with_context(
        body: &BeginEndStatements,
        mut context: PlPgSqlContext,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, Error> {
        let mut result = Vec::new();

        context.seed_default_bindings();

        // Process each statement in the body
        for stmt in &body.statements {
            let translated = Self::translate_statement(stmt, &mut context, schema, options)?;
            result.extend(translated);
        }

        Ok(result)
    }

    /// Translates a single statement within the function body.
    fn translate_statement(
        stmt: &Statement,
        context: &mut PlPgSqlContext,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, Error> {
        match stmt {
            // Handle SET statements (variable assignments from preprocessing)
            Statement::Set(set) => {
                Self::handle_set_statement(set, context);
                // SET statements don't produce output - they just record the binding
                Ok(vec![])
            }

            // Handle IF statements
            Statement::If(if_stmt) => {
                Self::translate_if_statement(if_stmt, context, schema, options)
            }

            // Handle INSERT statements
            Statement::Insert(insert) => {
                Self::translate_insert_statement(insert, context, schema, options)
            }

            // Handle Query statements (e.g., WITH RECURSIVE ... INSERT/DELETE)
            Statement::Query(query) => {
                Self::translate_query_statement(query, context, schema, options)
            }

            // Handle UPDATE and DELETE statements - both need condition injection
            Statement::Update(_) | Statement::Delete(_) => {
                let mut translated = stmt.translate(schema, options)?;
                // Inject condition if we're in an IF block
                if let Some(condition) = context.current_condition() {
                    for t_stmt in &mut translated {
                        Self::inject_condition_into_statement(t_stmt, &condition);
                    }
                }
                Ok(translated)
            }

            // Other statements - translate normally
            other => other.translate(schema, options),
        }
    }

    /// Handles a SET statement (variable assignment).
    fn handle_set_statement(set: &Set, context: &mut PlPgSqlContext) {
        if let Set::SingleAssignment { variable, values, .. } = set
            && values.len() == 1
        {
            let expression = values[0].to_string();
            let binding =
                VariableBinding { name: variable.to_string(), expression: expression.clone() };

            // If the expression is a subquery (from SELECT INTO), store as persistent
            // Otherwise, store as scoped (will be cleared per IF block)
            if expression.trim().starts_with('(') && expression.contains("SELECT") {
                context.add_persistent_binding(binding);
            } else {
                context.add_binding(binding);
            }
        }
    }

    /// Translates an IF statement, including ELSIF and ELSE blocks.
    fn translate_if_statement(
        if_stmt: &sqlparser::ast::IfStatement,
        context: &mut PlPgSqlContext,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, Error> {
        let mut result = Vec::new();

        // Track negated conditions for ELSIF/ELSE blocks
        let mut negated_conditions: Vec<String> = Vec::new();

        // Get the IF condition (translated to SQLite form)
        let if_condition = if_stmt.if_block.condition.as_ref().map_or_else(
            || "TRUE".to_string(),
            |cond| {
                cond.translate(schema, options).map_or_else(|_| cond.to_string(), |t| t.to_string())
            },
        );

        // Push IF condition onto stack
        context.push_condition(if_condition.clone());

        // Clear scoped bindings for this IF block (persistent bindings are kept)
        context.clear_scoped_bindings();

        // Clear UUID first-use tracking for this IF block
        context.clear_uuid_first_use();

        // Translate statements within the IF block
        for stmt in if_stmt.if_block.statements() {
            let translated = Self::translate_statement(stmt, context, schema, options)?;
            result.extend(translated);
        }

        // Pop IF condition from stack
        context.pop_condition();

        // Remember negation of IF condition for ELSIF/ELSE
        negated_conditions.push(format!("NOT ({if_condition})"));

        // Handle ELSIF blocks
        for elseif_block in &if_stmt.elseif_blocks {
            let elseif_condition = elseif_block.condition.as_ref().map_or_else(
                || "TRUE".to_string(),
                |cond| {
                    cond.translate(schema, options)
                        .map_or_else(|_| cond.to_string(), |t| t.to_string())
                },
            );

            // Build combined condition: NOT(prev conditions) AND this condition
            debug_assert!(
                !negated_conditions.is_empty(),
                "IF condition negation must be seeded before ELSIF translation"
            );
            let combined =
                format!("{} AND ({})", negated_conditions.join(" AND "), elseif_condition);

            context.push_condition(combined);
            context.clear_scoped_bindings();
            context.clear_uuid_first_use();

            for stmt in elseif_block.statements() {
                let translated = Self::translate_statement(stmt, context, schema, options)?;
                result.extend(translated);
            }

            context.pop_condition();

            // Add this condition's negation for subsequent blocks
            negated_conditions.push(format!("NOT ({elseif_condition})"));
        }

        // Handle ELSE block
        if let Some(else_block) = &if_stmt.else_block {
            // ELSE condition is negation of all previous conditions
            let else_condition = negated_conditions.join(" AND ");

            context.push_condition(else_condition);
            context.clear_scoped_bindings();
            context.clear_uuid_first_use();

            for stmt in else_block.statements() {
                let translated = Self::translate_statement(stmt, context, schema, options)?;
                result.extend(translated);
            }

            context.pop_condition();
        }

        Ok(result)
    }

    /// Translates a Query statement (e.g., WITH RECURSIVE ... INSERT/DELETE).
    ///
    /// SQLite does NOT support `WITH` clauses directly in trigger bodies.
    /// We must transform:
    ///   `WITH RECURSIVE cte AS (...) INSERT INTO t SELECT ... FROM cte`
    /// Into:
    ///   `INSERT INTO t SELECT ... FROM (WITH RECURSIVE cte AS (...) SELECT *
    /// FROM cte) AS subq`
    ///
    /// For DELETE with IN (SELECT ... FROM cte):
    ///   `WITH RECURSIVE cte AS (...) DELETE FROM t WHERE col IN (SELECT id
    /// FROM cte)` Stays as-is because the WITH is inside a subquery in the
    /// WHERE clause.
    fn translate_query_statement(
        query: &Query,
        context: &mut PlPgSqlContext,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, Error> {
        let transformed_statements = if let Some(with) = &query.with {
            // Check if this is a WITH ... INSERT pattern that needs transformation
            if let SetExpr::Insert(Statement::Insert(insert)) = &*query.body {
                Self::transform_with_insert_to_subquery(with, insert, context, options)?
            } else if let SetExpr::Delete(Statement::Delete(delete)) = &*query.body {
                Self::transform_with_delete_to_subquery(with, delete, context, options)
            } else {
                let transformed_body = Self::transform_query_body(&query.body, context, options)?;

                let transformed_with = query.with.as_ref().map(|with| {
                    let mut new_with = with.clone();
                    for cte in &mut new_with.cte_tables {
                        Self::transform_cte_query(&mut cte.query, context, options);
                    }
                    new_with
                });

                let transformed_query = Query {
                    with: transformed_with,
                    body: Box::new(transformed_body),
                    ..query.clone()
                };

                vec![Statement::Query(Box::new(transformed_query))]
            }
        } else {
            // For non-WITH query statements, transform normally
            let transformed_body = Self::transform_query_body(&query.body, context, options)?;
            let transformed_query =
                Query { with: None, body: Box::new(transformed_body), ..query.clone() };

            vec![Statement::Query(Box::new(transformed_query))]
        };

        Self::finalize_query_statements(transformed_statements, context, schema, options)
    }

    fn finalize_query_statements(
        statements: Vec<Statement>,
        context: &PlPgSqlContext,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, Error> {
        let mut finalized = Vec::new();
        let condition = context.current_condition();

        for statement in statements {
            let mut translated_statements = statement.translate(schema, options)?;

            if let Some(condition) = &condition {
                for translated in &mut translated_statements {
                    Self::inject_condition_into_statement(translated, condition);
                }
            }

            finalized.extend(translated_statements);
        }

        Ok(finalized)
    }

    /// Transforms `WITH RECURSIVE cte AS (...) INSERT INTO t SELECT ... FROM
    /// cte` into `INSERT INTO t SELECT * FROM (WITH RECURSIVE cte AS (...)
    /// SELECT ... FROM cte) AS subq`
    #[allow(clippy::too_many_lines)]
    fn transform_with_insert_to_subquery(
        with: &sqlparser::ast::With,
        insert: &sqlparser::ast::Insert,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, Error> {
        // Get the source SELECT from the INSERT
        let Some(source) = &insert.source else {
            return Err(Error::UnsupportedSQLiteFeature(
                "WITH ... INSERT without source SELECT not supported".to_string(),
            ));
        };

        // Create a new WITH query that wraps the original SELECT
        let inner_query = Query { with: Some(with.clone()), ..source.as_ref().clone() };

        // Transform NEW/OLD references in the inner query
        let mut transformed_inner = inner_query;
        Self::transform_cte_query(&mut transformed_inner, context, options);

        // Get the column list from the original SELECT to preserve
        let projection: Vec<SelectItem> = match &*source.body {
            SetExpr::Select(select) => select.projection.clone(),
            _ => vec![SelectItem::Wildcard(sqlparser::ast::WildcardAdditionalOptions::default())],
        };

        // Create a new SELECT that just selects * from the subquery
        // The inner query already has the right columns projected
        let subquery_alias = "recursive_cte_subquery";

        // We need to determine the column names from the inner projection
        // and select them from the subquery alias
        let outer_projection: Vec<SelectItem> = projection
            .iter()
            .enumerate()
            .map(|(i, item)| {
                match item {
                    SelectItem::UnnamedExpr(Expr::Identifier(ident)) => {
                        // Column reference - keep it, prepend alias
                        SelectItem::UnnamedExpr(Expr::CompoundIdentifier(vec![
                            Ident::new(subquery_alias.to_string()),
                            ident.clone(),
                        ]))
                    }
                    SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => {
                        // Compound identifier - just use the last part (column name)
                        parts.last().map_or_else(
                            || {
                                SelectItem::UnnamedExpr(Expr::Identifier(Ident::new(format!(
                                    "col{i}"
                                ))))
                            },
                            |col_name| {
                                SelectItem::UnnamedExpr(Expr::CompoundIdentifier(vec![
                                    Ident::new(subquery_alias.to_string()),
                                    col_name.clone(),
                                ]))
                            },
                        )
                    }
                    SelectItem::ExprWithAlias { alias, .. } => {
                        // Use the alias
                        SelectItem::UnnamedExpr(Expr::CompoundIdentifier(vec![
                            Ident::new(subquery_alias.to_string()),
                            alias.clone(),
                        ]))
                    }
                    _ => {
                        // For other cases (expressions, wildcards), use positional
                        SelectItem::UnnamedExpr(Expr::Identifier(Ident::new(format!("col{i}"))))
                    }
                }
            })
            .collect();

        // Simpler approach: just use SELECT * FROM (subquery)
        let use_wildcard = outer_projection.iter().any(|item| {
            matches!(item, SelectItem::UnnamedExpr(Expr::Identifier(id)) if id.value.starts_with("col"))
        });

        let final_projection = if use_wildcard {
            vec![SelectItem::Wildcard(sqlparser::ast::WildcardAdditionalOptions::default())]
        } else {
            outer_projection
        };

        let new_select = make_simple_select(
            final_projection,
            vec![TableWithJoins {
                relation: TableFactor::Derived {
                    lateral: false,
                    subquery: Box::new(transformed_inner),
                    alias: Some(TableAlias {
                        name: Ident::new(subquery_alias.to_string()),
                        columns: vec![],
                        explicit: false,
                        at: None,
                    }),
                    sample: None,
                },
                joins: vec![],
            }],
            None,
        );

        // Build new source query
        let new_source = make_query(None, SetExpr::Select(Box::new(new_select)));

        // Build the new INSERT, handling ON CONFLICT DO NOTHING -> OR IGNORE
        let mut new_insert = insert.clone();
        new_insert.source = Some(Box::new(new_source));

        // Check for ON CONFLICT DO NOTHING and convert to OR IGNORE
        if let Some(sqlparser::ast::OnInsert::OnConflict(conflict)) = &insert.on
            && conflict.action == sqlparser::ast::OnConflictAction::DoNothing
        {
            // Set INSERT OR IGNORE and remove the ON CONFLICT clause
            new_insert.or = Some(sqlparser::ast::SqliteOnConflict::Ignore);
            new_insert.on = None;
        }

        Ok(vec![Statement::Insert(new_insert)])
    }

    /// Transforms `WITH RECURSIVE cte AS (...) DELETE FROM t WHERE ... IN
    /// (SELECT FROM cte)` The CTE needs to be moved inside the IN subquery.
    fn transform_with_delete_to_subquery(
        with: &sqlparser::ast::With,
        delete: &sqlparser::ast::Delete,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) -> Vec<Statement> {
        let mut new_delete = delete.clone();

        // Transform the selection - move WITH into IN subqueries
        if let Some(selection) = &mut new_delete.selection {
            Self::inject_with_into_in_subqueries(selection, with, context, options);
        }

        vec![Statement::Delete(new_delete)]
    }

    /// Injects a WITH clause into IN subqueries that reference CTEs from that
    /// WITH.
    fn inject_with_into_in_subqueries(
        expr: &mut Expr,
        with: &sqlparser::ast::With,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) {
        use crate::impls::expr_helpers::mutate_expr_children;

        /// Inject CTE into a subquery if it references any of the CTEs.
        fn maybe_inject_with(
            subquery: &mut Query,
            with: &sqlparser::ast::With,
            context: &mut PlPgSqlContext,
            options: &Pg2SqliteOptions,
        ) {
            let cte_names: Vec<String> =
                with.cte_tables.iter().map(|cte| cte.alias.name.value.clone()).collect();
            if PlPgSqlTranslator::query_references_ctes(subquery, &cte_names) {
                let mut new_with = with.clone();
                for cte in &mut new_with.cte_tables {
                    PlPgSqlTranslator::transform_cte_query(&mut cte.query, context, options);
                }
                subquery.with = Some(new_with);
            }
        }

        match expr {
            Expr::InSubquery { subquery, .. }
            | Expr::Subquery(subquery)
            | Expr::Exists { subquery, .. } => {
                maybe_inject_with(subquery, with, context, options);
            }
            Expr::Function(func) => {
                if let FunctionArguments::List(arg_list) = &mut func.args {
                    for arg in &mut arg_list.args {
                        let arg_expr = match arg {
                            FunctionArg::Unnamed(e)
                            | FunctionArg::Named { arg: e, .. }
                            | FunctionArg::ExprNamed { arg: e, .. } => e,
                        };
                        if let FunctionArgExpr::Expr(e) = arg_expr {
                            Self::inject_with_into_in_subqueries(e, with, context, options);
                        }
                    }
                }
            }
            other => {
                mutate_expr_children(other, &mut |child| {
                    Self::inject_with_into_in_subqueries(child, with, context, options);
                });
            }
        }
    }

    /// Checks if a query references any of the given CTE names via AST
    /// traversal.
    ///
    /// This walks the FROM clauses and subqueries at the AST level to avoid
    /// false positives from string-contains matching (e.g., a column name that
    /// is a substring of a CTE name).
    fn query_references_ctes(query: &Query, cte_names: &[String]) -> bool {
        let normalized: Vec<String> = cte_names.iter().map(|n| n.to_lowercase()).collect();
        Self::set_expr_references_ctes(&query.body, &normalized)
    }

    /// Recursively checks if a `SetExpr` references any of the CTE names.
    fn set_expr_references_ctes(body: &SetExpr, cte_names: &[String]) -> bool {
        match body {
            SetExpr::Select(select) => Self::select_references_ctes(select, cte_names),
            SetExpr::SetOperation { left, right, .. } => {
                Self::set_expr_references_ctes(left, cte_names)
                    || Self::set_expr_references_ctes(right, cte_names)
            }
            _ => false,
        }
    }

    /// Checks if a `Select` references any CTE by looking at FROM clauses.
    fn select_references_ctes(select: &Select, cte_names: &[String]) -> bool {
        select.from.iter().any(|from| Self::table_with_joins_references_ctes(from, cte_names))
    }

    /// Checks if a `TableWithJoins` references any CTE.
    fn table_with_joins_references_ctes(twj: &TableWithJoins, cte_names: &[String]) -> bool {
        Self::table_factor_references_ctes(&twj.relation, cte_names)
            || twj.joins.iter().any(|j| Self::table_factor_references_ctes(&j.relation, cte_names))
    }

    /// Checks if a `TableFactor` references any CTE.
    fn table_factor_references_ctes(tf: &TableFactor, cte_names: &[String]) -> bool {
        match tf {
            TableFactor::Table { name, .. } => {
                if let Some(last) = name.0.last().and_then(|p| p.as_ident()) {
                    cte_names.contains(&last.value.to_lowercase())
                } else {
                    false
                }
            }
            TableFactor::Derived { subquery, .. } => {
                Self::set_expr_references_ctes(&subquery.body, cte_names)
            }
            _ => false,
        }
    }

    /// Transforms the body of a Query (INSERT, DELETE, SELECT, etc.).
    #[allow(clippy::unnecessary_wraps)]
    fn transform_query_body(
        body: &SetExpr,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) -> Result<SetExpr, Error> {
        match body {
            SetExpr::Insert(Statement::Insert(insert)) => {
                let transformed = Self::transform_insert_for_sqlite(insert, context, options);
                Ok(SetExpr::Insert(Statement::Insert(transformed)))
            }
            SetExpr::Delete(Statement::Delete(delete)) => {
                let transformed = Self::transform_delete_for_sqlite(delete, context, options);
                Ok(SetExpr::Delete(Statement::Delete(transformed)))
            }
            other => {
                let mut cloned = other.clone();
                Self::transform_set_expr(&mut cloned, context, options);
                Ok(cloned)
            }
        }
    }

    /// Applies the standard query-expression rewrites used by both CTE and
    /// subquery transformation paths.
    fn transform_query_expressions(
        query: &mut Query,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) {
        if let Ok(transformed_body) = Self::transform_query_body(&query.body, context, options) {
            *query.body = transformed_body;
        }
        if let Some(with) = &mut query.with {
            for cte in &mut with.cte_tables {
                Self::transform_cte_query(&mut cte.query, context, options);
            }
        }
        if let Some(order_by) = &mut query.order_by
            && let sqlparser::ast::OrderByKind::Expressions(exprs) = &mut order_by.kind
        {
            for ob in exprs {
                Self::transform_expr(&mut ob.expr, context, options);
            }
        }
        if let Some(limit_clause) = &mut query.limit_clause {
            match limit_clause {
                sqlparser::ast::LimitClause::LimitOffset { limit, offset, limit_by } => {
                    if let Some(expr) = limit {
                        Self::transform_expr(expr, context, options);
                    }
                    if let Some(off) = offset {
                        Self::transform_expr(&mut off.value, context, options);
                    }
                    for expr in limit_by {
                        Self::transform_expr(expr, context, options);
                    }
                }
                sqlparser::ast::LimitClause::OffsetCommaLimit { offset, limit } => {
                    Self::transform_expr(offset, context, options);
                    Self::transform_expr(limit, context, options);
                }
            }
        }
        if let Some(fetch) = &mut query.fetch
            && let Some(expr) = &mut fetch.quantity
        {
            Self::transform_expr(expr, context, options);
        }
    }

    /// Transforms a CTE query (used in WITH clauses).
    fn transform_cte_query(
        query: &mut Query,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) {
        Self::transform_query_expressions(query, context, options);
    }

    /// Transforms an INSERT statement for SQLite compatibility.
    fn transform_insert_for_sqlite(
        insert: &sqlparser::ast::Insert,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) -> sqlparser::ast::Insert {
        let mut new_insert = insert.clone();

        // Transform source query if present
        if let Some(source) = &mut new_insert.source {
            if let Ok(transformed_body) = Self::transform_query_body(&source.body, context, options)
            {
                *source.body = transformed_body;
            }
            // Also transform VALUES - handle NEW/OLD references and UUID functions
            Self::transform_set_expr(&mut source.body, context, options);
        }

        new_insert
    }

    /// Transforms a DELETE statement for SQLite compatibility.
    fn transform_delete_for_sqlite(
        delete: &sqlparser::ast::Delete,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) -> sqlparser::ast::Delete {
        let mut new_delete = delete.clone();

        // Transform selection (WHERE clause)
        Self::transform_selection(&mut new_delete.selection, context, options);

        // Transform FROM clause (FromTable is an enum wrapping Vec<TableWithJoins>)
        match &mut new_delete.from {
            sqlparser::ast::FromTable::WithFromKeyword(tables)
            | sqlparser::ast::FromTable::WithoutKeyword(tables) => {
                for from in tables {
                    Self::transform_table_with_joins(from, context, options);
                }
            }
        }

        new_delete
    }

    /// Transforms a SetExpr, handling NEW/OLD references and UUID functions.
    fn transform_set_expr(
        set_expr: &mut SetExpr,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) {
        match set_expr {
            SetExpr::Select(select) => {
                // Transform projection items
                for item in &mut select.projection {
                    if let SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } =
                        item
                    {
                        Self::transform_expr(expr, context, options);
                    }
                }
                // Transform selection
                Self::transform_selection(&mut select.selection, context, options);
                // Transform FROM clauses
                for from in &mut select.from {
                    Self::transform_table_with_joins(from, context, options);
                }
                // Transform HAVING clause
                if let Some(having) = &mut select.having {
                    Self::transform_expr(having, context, options);
                }
                // Transform GROUP BY expressions
                if let GroupByExpr::Expressions(exprs, _) = &mut select.group_by {
                    for e in exprs {
                        Self::transform_expr(e, context, options);
                    }
                }
            }
            SetExpr::Values(values) => {
                for row in &mut values.rows {
                    for expr in &mut row.content {
                        Self::transform_expr(expr, context, options);
                    }
                }
            }
            SetExpr::SetOperation { left, right, .. } => {
                Self::transform_set_expr(left, context, options);
                Self::transform_set_expr(right, context, options);
            }
            SetExpr::Query(query) => {
                Self::transform_cte_query(query, context, options);
            }
            _ => {}
        }
    }

    /// Transforms table WITH JOINS in FROM clause.
    fn transform_table_with_joins(
        table_with_joins: &mut TableWithJoins,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) {
        // Transform the main table
        Self::transform_table_factor(&mut table_with_joins.relation, context, options);

        // Transform joined tables
        for join in &mut table_with_joins.joins {
            Self::transform_table_factor(&mut join.relation, context, options);
            // Transform join condition
            if let Some(sqlparser::ast::JoinConstraint::On(expr)) =
                crate::impls::shared_helpers::join_constraint_mut(&mut join.join_operator)
            {
                Self::transform_expr(expr, context, options);
            }
        }
    }

    /// Transforms a table factor (table reference).
    fn transform_table_factor(
        factor: &mut TableFactor,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) {
        match factor {
            TableFactor::Derived { subquery, .. } => {
                Self::transform_cte_query(subquery, context, options);
            }
            TableFactor::NestedJoin { table_with_joins, .. } => {
                Self::transform_table_with_joins(table_with_joins, context, options);
            }
            _ => {}
        }
    }

    /// Transforms an optional selection (WHERE clause).
    fn transform_selection(
        selection: &mut Option<Expr>,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) {
        if let Some(expr) = selection {
            Self::transform_expr(expr, context, options);
        }
    }

    /// Transforms an expression, handling NEW/OLD references and UUID
    /// functions.
    #[allow(clippy::too_many_lines)]
    fn transform_expr(expr: &mut Expr, context: &mut PlPgSqlContext, options: &Pg2SqliteOptions) {
        use crate::impls::expr_helpers::mutate_expr_children;

        match expr {
            Expr::CompoundIdentifier(parts)
                if parts.len() == 2
                    && (parts[0].value.eq_ignore_ascii_case("NEW")
                        || parts[0].value.eq_ignore_ascii_case("OLD")) =>
            {
                // Keep as-is, SQLite supports NEW.col and OLD.col in triggers
            }
            Expr::Function(func) => {
                // Transform UUID function names
                let func_name = func.name.0.last().and_then(|part| part.as_ident()).map_or_else(
                    || func.name.to_string().to_ascii_lowercase(),
                    |ident| ident.value.to_ascii_lowercase(),
                );
                if matches!(
                    func_name.as_str(),
                    "gen_random_uuid" | "uuid_generate_v4" | "uuidv4" | "uuidv7"
                ) {
                    let new_name = options.get_uuid_function_name();
                    func.name = ObjectName(vec![ObjectNamePart::Identifier(Ident::new(new_name))]);
                }
                Self::transform_function_parts(func, context, options);
            }
            Expr::InSubquery { expr: inner, subquery, .. } => {
                Self::transform_expr(inner, context, options);
                Self::transform_subquery(subquery, context, options);
            }
            Expr::Subquery(subquery) | Expr::Exists { subquery, .. } => {
                Self::transform_subquery(subquery, context, options);
            }
            other => {
                mutate_expr_children(other, &mut |child| {
                    Self::transform_expr(child, context, options);
                });
            }
        }
    }

    /// Transform function arguments, parameters, filter, over, and
    /// within_group.
    fn transform_function_parts(
        func: &mut sqlparser::ast::Function,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) {
        let transform_func_args =
            |args: &mut FunctionArguments, ctx: &mut PlPgSqlContext, opts: &Pg2SqliteOptions| {
                if let FunctionArguments::List(list) = args {
                    for arg in &mut list.args {
                        match arg {
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                            | FunctionArg::Named { arg: FunctionArgExpr::Expr(e), .. }
                            | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(e), .. } => {
                                Self::transform_expr(e, ctx, opts);
                            }
                            _ => {}
                        }
                    }
                }
            };
        transform_func_args(&mut func.args, context, options);
        transform_func_args(&mut func.parameters, context, options);
        if let Some(filter) = &mut func.filter {
            Self::transform_expr(filter, context, options);
        }
        if let Some(sqlparser::ast::WindowType::WindowSpec(spec)) = &mut func.over {
            for e in &mut spec.partition_by {
                Self::transform_expr(e, context, options);
            }
            for ob in &mut spec.order_by {
                Self::transform_expr(&mut ob.expr, context, options);
            }
        }
        for ob in &mut func.within_group {
            Self::transform_expr(&mut ob.expr, context, options);
        }
    }

    /// Transform a subquery's body and query-level expressions.
    fn transform_subquery(
        subquery: &mut Query,
        context: &mut PlPgSqlContext,
        options: &Pg2SqliteOptions,
    ) {
        Self::transform_query_expressions(subquery, context, options);
    }

    /// Translates an INSERT statement with CTE injection for variables.
    ///
    /// Uses the `last_insert_rowid()` pattern for UUID variables:
    /// - First INSERT using a UUID variable: uses the expression directly
    ///   (e.g., `uuidv7()`)
    /// - Subsequent INSERTTs: use `SELECT col FROM table WHERE rowid =
    ///   last_insert_rowid()`
    #[allow(clippy::too_many_lines)]
    fn translate_insert_statement(
        insert: &sqlparser::ast::Insert,
        context: &mut PlPgSqlContext,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, Error> {
        use sqlparser::ast::TableObject;

        // Get bindings and condition
        let bindings: Vec<_> = context.bindings().cloned().collect();
        let condition = context.current_condition();

        // If no bindings and no condition, use standard translation
        if bindings.is_empty() && condition.is_none() {
            return Statement::Insert(insert.clone()).translate(schema, options);
        }

        // Get table name from INSERT
        let table_name = match &insert.table {
            TableObject::TableName(object_name) => {
                object_name
                    .0
                    .last()
                    .map(|p| {
                        match p {
                            ObjectNamePart::Identifier(ident) => ident.value.clone(),
                            ObjectNamePart::Function(_) => String::new(),
                        }
                    })
                    .unwrap_or_default()
            }
            TableObject::TableFunction(_) | TableObject::TableQuery(_) => {
                return Err(Error::UnsupportedSQLiteFeature(
                    "INSERT into table function not supported".to_string(),
                ));
            }
        };

        // Get column names from INSERT
        let column_names: Vec<String> = insert
            .columns
            .iter()
            .map(|c| {
                c.0.last()
                    .and_then(sqlparser::ast::ObjectNamePart::as_ident)
                    .map_or_else(|| c.to_string(), |id| id.value.clone())
            })
            .collect();

        // Process bindings - check which UUID variables need the last_insert_rowid()
        // pattern
        let mut modified_bindings = Vec::new();
        let mut uuid_var_to_column: Vec<(String, String)> = Vec::new();

        for binding in &bindings {
            let is_uuid_gen = {
                let expr_lower = binding.expression.to_lowercase();
                expr_lower.contains("uuidv7()")
                    || expr_lower.contains("uuidv4()")
                    || expr_lower.contains("uuid_generate_v4()")
                    || expr_lower.contains("gen_random_uuid()")
            };

            if is_uuid_gen {
                // Check if this variable was already used in a previous INSERT
                if let Some(first_use) = context.get_uuid_first_use(&binding.name) {
                    // Use last_insert_rowid() to get the value from the previous INSERT
                    let new_expr = format!(
                        "(SELECT {} FROM {} WHERE rowid = last_insert_rowid())",
                        first_use.column_name, first_use.table_name
                    );
                    modified_bindings
                        .push(VariableBinding { name: binding.name.clone(), expression: new_expr });
                } else {
                    // First use - find which column this variable is being inserted into
                    // and record it for future INSERTTs
                    if let Some(source) = &insert.source
                        && let SetExpr::Values(values) = &*source.body
                        && !values.rows.is_empty()
                    {
                        let row = &values.rows[0];
                        for (i, expr) in row.iter().enumerate() {
                            if Self::expr_references_variable(expr, &binding.name)
                                && i < column_names.len()
                            {
                                uuid_var_to_column
                                    .push((binding.name.clone(), column_names[i].clone()));
                            }
                        }
                    }
                    modified_bindings.push(binding.clone());
                }
            } else {
                modified_bindings.push(binding.clone());
            }
        }

        // Record first use of UUID variables for this INSERT
        for (var_name, col_name) in &uuid_var_to_column {
            if context.get_uuid_first_use(var_name).is_none() {
                context.record_uuid_first_use(var_name, &table_name, col_name);
            }
        }

        let mut new_insert = insert.clone();

        // Collect CTEs for variable bindings
        let mut ctes = Vec::new();

        for binding in &modified_bindings {
            // Translate UUID function names in the expression
            let translated_expr = Self::translate_uuid_function(&binding.expression, options);
            // Parse the expression
            let expr = Self::parse_expression(&translated_expr)?;
            // Translate PG-specific constructs in the CTE expression
            let expr = expr.translate(schema, options).unwrap_or(expr);
            let cte = CteBuilder::create_variable_cte(binding, expr);
            ctes.push(cte);
        }

        // Get the current condition (if any)
        let condition = context.current_condition();

        // Transform the INSERT
        if let Some(source) = &insert.source {
            match &*source.body {
                SetExpr::Values(values) => {
                    // Transform VALUES into SELECT with variable substitution
                    let new_source = Self::transform_values_to_select(
                        values,
                        &modified_bindings,
                        condition.as_deref(),
                    )?;

                    // Build the new query with CTEs
                    new_insert.source = Some(Box::new(Query {
                        with: CteBuilder::combine_ctes(ctes),
                        body: Box::new(new_source),
                        order_by: None,
                        limit_clause: None,
                        fetch: None,
                        locks: vec![],
                        for_clause: None,
                        settings: None,
                        format_clause: None,
                        pipe_operators: vec![],
                    }));
                }
                SetExpr::Select(select) => {
                    // Already a SELECT - add CTEs and condition
                    let mut new_select = select.as_ref().clone();

                    // Add condition if present
                    if let Some(cond) = &condition {
                        let cond_expr = Self::parse_expression(cond)?;
                        new_select.selection = match &new_select.selection {
                            Some(existing) => {
                                Some(Expr::BinaryOp {
                                    left: Box::new(existing.clone()),
                                    op: BinaryOperator::And,
                                    right: Box::new(cond_expr),
                                })
                            }
                            None => Some(cond_expr),
                        };
                    }

                    new_insert.source = Some(Box::new(Query {
                        with: CteBuilder::combine_ctes(ctes),
                        body: Box::new(SetExpr::Select(Box::new(new_select))),
                        ..source.as_ref().clone()
                    }));
                }
                _ => {
                    // Other source types - translate normally
                    return Statement::Insert(insert.clone()).translate(schema, options);
                }
            }
        }

        // Apply standard translation (e.g., UUID handling)
        Statement::Insert(new_insert).translate(schema, options)
    }

    fn function_arg_references_variable(arg: &FunctionArg, var_name: &str) -> bool {
        match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(inner))
            | FunctionArg::Named { arg: FunctionArgExpr::Expr(inner), .. }
            | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(inner), .. } => {
                Self::expr_references_variable(inner, var_name)
            }
            _ => false,
        }
    }

    fn function_references_variable(func: &sqlparser::ast::Function, var_name: &str) -> bool {
        let args_have_var = match &func.args {
            FunctionArguments::List(arg_list) => {
                arg_list
                    .args
                    .iter()
                    .any(|arg| Self::function_arg_references_variable(arg, var_name))
            }
            _ => false,
        };
        let filter_has_var = func
            .filter
            .as_ref()
            .is_some_and(|filter| Self::expr_references_variable(filter, var_name));
        let over_has_var = matches!(&func.over, Some(sqlparser::ast::WindowType::WindowSpec(window_spec))
            if window_spec
                .partition_by
                .iter()
                .any(|expr| Self::expr_references_variable(expr, var_name))
                || window_spec
                    .order_by
                    .iter()
                    .any(|order| Self::expr_references_variable(&order.expr, var_name)));
        let within_group_has_var = func
            .within_group
            .iter()
            .any(|order| Self::expr_references_variable(&order.expr, var_name));

        args_have_var || filter_has_var || over_has_var || within_group_has_var
    }

    /// Checks if an expression references a specific variable name.
    fn expr_references_variable(expr: &Expr, var_name: &str) -> bool {
        match expr {
            Expr::Identifier(ident) => ident.value == var_name,
            Expr::CompoundIdentifier(idents) => idents.iter().any(|i| i.value == var_name),
            Expr::Function(func) => Self::function_references_variable(func, var_name),
            Expr::Subquery(subquery) | Expr::Exists { subquery, .. } => {
                Self::query_references_variable_expr(subquery, var_name)
            }
            Expr::InSubquery { expr: inner, subquery, .. } => {
                Self::expr_references_variable(inner, var_name)
                    || Self::query_references_variable_expr(subquery, var_name)
            }
            _ => any_child_expr(expr, &|child| Self::expr_references_variable(child, var_name)),
        }
    }

    /// Checks whether a query expression references a specific variable.
    fn query_references_variable_expr(query: &Query, var_name: &str) -> bool {
        let body_has_var = Self::set_expr_references_variable_expr(&query.body, var_name);
        let order_by_has_var = query.order_by.as_ref().is_some_and(|order_by| {
            let kind_has_var = match &order_by.kind {
                sqlparser::ast::OrderByKind::Expressions(exprs) => {
                    exprs.iter().any(|order_expr| {
                        Self::expr_references_variable(&order_expr.expr, var_name)
                            || order_expr.with_fill.as_ref().is_some_and(|with_fill| {
                                with_fill.from.as_ref().is_some_and(|expr| {
                                    Self::expr_references_variable(expr, var_name)
                                }) || with_fill.to.as_ref().is_some_and(|expr| {
                                    Self::expr_references_variable(expr, var_name)
                                }) || with_fill.step.as_ref().is_some_and(|expr| {
                                    Self::expr_references_variable(expr, var_name)
                                })
                            })
                    })
                }
                sqlparser::ast::OrderByKind::All(_) => false,
            };
            let interpolate_has_var = order_by.interpolate.as_ref().is_some_and(|interpolate| {
                interpolate.exprs.as_ref().is_some_and(|exprs| {
                    exprs.iter().any(|interpolate_expr| {
                        interpolate_expr
                            .expr
                            .as_ref()
                            .is_some_and(|expr| Self::expr_references_variable(expr, var_name))
                    })
                })
            });

            kind_has_var || interpolate_has_var
        });
        let limit_has_var = query.limit_clause.as_ref().is_some_and(|limit_clause| {
            match limit_clause {
                sqlparser::ast::LimitClause::LimitOffset { limit, offset, limit_by } => {
                    limit
                        .as_ref()
                        .is_some_and(|expr| Self::expr_references_variable(expr, var_name))
                        || offset.as_ref().is_some_and(|offset_expr| {
                            Self::expr_references_variable(&offset_expr.value, var_name)
                        })
                        || limit_by
                            .iter()
                            .any(|expr| Self::expr_references_variable(expr, var_name))
                }
                sqlparser::ast::LimitClause::OffsetCommaLimit { offset, limit } => {
                    Self::expr_references_variable(offset, var_name)
                        || Self::expr_references_variable(limit, var_name)
                }
            }
        });
        let fetch_has_var = query.fetch.as_ref().is_some_and(|fetch| {
            fetch
                .quantity
                .as_ref()
                .is_some_and(|expr| Self::expr_references_variable(expr, var_name))
        });

        body_has_var || order_by_has_var || limit_has_var || fetch_has_var
    }

    /// Checks whether a `SetExpr` references a specific variable.
    fn set_expr_references_variable_expr(set_expr: &SetExpr, var_name: &str) -> bool {
        match set_expr {
            SetExpr::Select(select) => {
                let projection_has_var = select.projection.iter().any(|item| {
                    match item {
                        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                            Self::expr_references_variable(expr, var_name)
                        }
                        _ => false,
                    }
                });
                let selection_has_var = select
                    .selection
                    .as_ref()
                    .is_some_and(|expr| Self::expr_references_variable(expr, var_name));
                let having_has_var = select
                    .having
                    .as_ref()
                    .is_some_and(|expr| Self::expr_references_variable(expr, var_name));
                let group_by_has_var = match &select.group_by {
                    GroupByExpr::All(_) => false,
                    GroupByExpr::Expressions(exprs, _) => {
                        exprs.iter().any(|expr| Self::expr_references_variable(expr, var_name))
                    }
                };
                let from_has_var = select
                    .from
                    .iter()
                    .any(|table| Self::table_with_joins_references_variable(table, var_name));

                projection_has_var
                    || selection_has_var
                    || having_has_var
                    || group_by_has_var
                    || from_has_var
            }
            SetExpr::Values(values) => {
                values.rows.iter().any(|row| {
                    row.iter().any(|expr| Self::expr_references_variable(expr, var_name))
                })
            }
            SetExpr::SetOperation { left, right, .. } => {
                Self::set_expr_references_variable_expr(left, var_name)
                    || Self::set_expr_references_variable_expr(right, var_name)
            }
            SetExpr::Query(query) => Self::query_references_variable_expr(query, var_name),
            _ => false,
        }
    }

    /// Checks whether a table reference with joins references a specific
    /// variable.
    fn table_with_joins_references_variable(table: &TableWithJoins, var_name: &str) -> bool {
        Self::table_factor_references_variable(&table.relation, var_name)
            || table.joins.iter().any(|join| {
                Self::table_factor_references_variable(&join.relation, var_name) || {
                    let constraint_refs = matches!(
                        crate::impls::shared_helpers::join_constraint_ref(&join.join_operator),
                        Some(sqlparser::ast::JoinConstraint::On(expr))
                            if Self::expr_references_variable(expr, var_name)
                    );
                    let match_refs = matches!(
                        &join.join_operator,
                        sqlparser::ast::JoinOperator::AsOf { match_condition, .. }
                            if Self::expr_references_variable(match_condition, var_name)
                    );
                    constraint_refs || match_refs
                }
            })
    }

    /// Checks whether a table factor references a specific variable.
    fn table_factor_references_variable(factor: &TableFactor, var_name: &str) -> bool {
        match factor {
            TableFactor::Derived { subquery, .. } => {
                Self::query_references_variable_expr(subquery, var_name)
            }
            _ => false,
        }
    }

    /// Transforms VALUES clause into a SELECT with variable substitution.
    #[allow(clippy::too_many_lines)]
    fn transform_values_to_select(
        values: &sqlparser::ast::Values,
        bindings: &[VariableBinding],
        condition: Option<&str>,
    ) -> Result<SetExpr, Error> {
        if values.rows.len() != 1 {
            return Err(Error::UnknownPostgresFeature(
                "Multi-row VALUES in trigger not supported".into(),
            ));
        }

        let row = &values.rows[0];
        let mut projections = Vec::new();

        // Transform each value, substituting variable references
        for expr in &row.content {
            let substituted = Self::substitute_variables(expr, bindings);
            projections.push(SelectItem::UnnamedExpr(substituted));
        }

        // Build FROM clause with CTE references
        let mut from_tables = Vec::new();
        for binding in bindings {
            from_tables.push(TableWithJoins {
                relation: TableFactor::Table {
                    name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(
                        binding.name.clone(),
                    ))]),
                    alias: None,
                    args: None,
                    with_hints: vec![],
                    version: None,
                    partitions: vec![],
                    json_path: None,
                    sample: None,
                    index_hints: vec![],
                    with_ordinality: false,
                },
                joins: vec![],
            });
        }

        // If no CTEs, use a dummy subquery
        if from_tables.is_empty() {
            from_tables.push(TableWithJoins {
                relation: TableFactor::Derived {
                    lateral: false,
                    sample: None,
                    subquery: Box::new(make_query(
                        None,
                        SetExpr::Select(Box::new(make_simple_select(
                            vec![SelectItem::UnnamedExpr(Expr::Value(ValueWithSpan {
                                value: Value::Number("1".to_string(), false),
                                span: Span::empty(),
                            }))],
                            vec![],
                            None,
                        ))),
                    )),
                    alias: Some(TableAlias {
                        name: Ident::new("_dummy".to_string()),
                        columns: vec![],
                        explicit: false,
                        at: None,
                    }),
                },
                joins: vec![],
            });
        }

        // Parse condition if present
        let selection =
            if let Some(cond) = condition { Some(Self::parse_expression(cond)?) } else { None };

        Ok(SetExpr::Select(Box::new(make_simple_select(projections, from_tables, selection))))
    }

    fn substitute_bound_variable(name: &str, bindings: &[VariableBinding]) -> Option<Expr> {
        bindings
            .iter()
            .any(|binding| name == binding.name)
            .then(|| CteBuilder::variable_reference(name))
    }

    fn substitute_function(func: &sqlparser::ast::Function, bindings: &[VariableBinding]) -> Expr {
        let mut rewritten = func.clone();
        if let FunctionArguments::List(arg_list) = &mut rewritten.args {
            for arg in &mut arg_list.args {
                match arg {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(inner))
                    | FunctionArg::Named { arg: FunctionArgExpr::Expr(inner), .. }
                    | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(inner), .. } => {
                        *inner = Self::substitute_variables(inner, bindings);
                    }
                    _ => {}
                }
            }
        }
        if let Some(filter) = &mut rewritten.filter {
            **filter = Self::substitute_variables(filter, bindings);
        }
        if let Some(over) = &mut rewritten.over
            && let sqlparser::ast::WindowType::WindowSpec(window_spec) = over
        {
            for partition_expr in &mut window_spec.partition_by {
                *partition_expr = Self::substitute_variables(partition_expr, bindings);
            }
            for order_by_expr in &mut window_spec.order_by {
                order_by_expr.expr = Self::substitute_variables(&order_by_expr.expr, bindings);
            }
        }
        for order_by_expr in &mut rewritten.within_group {
            order_by_expr.expr = Self::substitute_variables(&order_by_expr.expr, bindings);
        }
        Expr::Function(rewritten)
    }

    /// Substitutes variable references in an expression with CTE column
    /// references.
    fn substitute_variables(expr: &Expr, bindings: &[VariableBinding]) -> Expr {
        let recurse = |e: &Expr| Self::substitute_variables(e, bindings);

        match expr {
            Expr::Identifier(ident) => {
                Self::substitute_bound_variable(&ident.value, bindings)
                    .unwrap_or_else(|| expr.clone())
            }
            Expr::CompoundIdentifier(idents) if idents.len() == 1 => {
                Self::substitute_bound_variable(&idents[0].value, bindings)
                    .unwrap_or_else(|| expr.clone())
            }
            Expr::Function(func) => Self::substitute_function(func, bindings),
            Expr::InSubquery { expr: inner, subquery, negated } => {
                Expr::InSubquery {
                    expr: Box::new(recurse(inner)),
                    subquery: Box::new(Self::substitute_variables_in_query(subquery, bindings)),
                    negated: *negated,
                }
            }
            Expr::Subquery(subquery) => {
                Expr::Subquery(Box::new(Self::substitute_variables_in_query(subquery, bindings)))
            }
            Expr::Exists { subquery, negated } => {
                Expr::Exists {
                    subquery: Box::new(Self::substitute_variables_in_query(subquery, bindings)),
                    negated: *negated,
                }
            }
            other => map_expr_children(other, &recurse),
        }
    }

    /// Rewrites bound variable references recursively within a query.
    fn substitute_variables_in_query(query: &Query, bindings: &[VariableBinding]) -> Query {
        let mut rewritten = query.clone();
        rewritten.body = Box::new(Self::substitute_variables_in_set_expr(&query.body, bindings));
        if let Some(order_by) = &mut rewritten.order_by {
            match &mut order_by.kind {
                sqlparser::ast::OrderByKind::Expressions(exprs) => {
                    for order_expr in exprs {
                        order_expr.expr = Self::substitute_variables(&order_expr.expr, bindings);
                        if let Some(with_fill) = &mut order_expr.with_fill {
                            if let Some(from) = &mut with_fill.from {
                                *from = Self::substitute_variables(from, bindings);
                            }
                            if let Some(to) = &mut with_fill.to {
                                *to = Self::substitute_variables(to, bindings);
                            }
                            if let Some(step) = &mut with_fill.step {
                                *step = Self::substitute_variables(step, bindings);
                            }
                        }
                    }
                }
                sqlparser::ast::OrderByKind::All(_) => {}
            }
            if let Some(interpolate) = &mut order_by.interpolate
                && let Some(exprs) = &mut interpolate.exprs
            {
                for interpolate_expr in exprs {
                    if let Some(expr) = &mut interpolate_expr.expr {
                        *expr = Self::substitute_variables(expr, bindings);
                    }
                }
            }
        }
        if let Some(limit_clause) = &mut rewritten.limit_clause {
            match limit_clause {
                sqlparser::ast::LimitClause::LimitOffset { limit, offset, limit_by } => {
                    if let Some(limit_expr) = limit {
                        *limit_expr = Self::substitute_variables(limit_expr, bindings);
                    }
                    if let Some(offset_expr) = offset {
                        offset_expr.value =
                            Self::substitute_variables(&offset_expr.value, bindings);
                    }
                    for expr in limit_by {
                        *expr = Self::substitute_variables(expr, bindings);
                    }
                }
                sqlparser::ast::LimitClause::OffsetCommaLimit { offset, limit } => {
                    *offset = Self::substitute_variables(offset, bindings);
                    *limit = Self::substitute_variables(limit, bindings);
                }
            }
        }
        if let Some(fetch) = &mut rewritten.fetch
            && let Some(quantity) = &mut fetch.quantity
        {
            *quantity = Self::substitute_variables(quantity, bindings);
        }
        rewritten
    }

    /// Rewrites bound variable references recursively within a `SetExpr`.
    fn substitute_variables_in_set_expr(
        set_expr: &SetExpr,
        bindings: &[VariableBinding],
    ) -> SetExpr {
        match set_expr {
            SetExpr::Select(select) => {
                let mut rewritten = (**select).clone();
                for item in &mut rewritten.projection {
                    if let SelectItem::UnnamedExpr(inner)
                    | SelectItem::ExprWithAlias { expr: inner, .. } = item
                    {
                        *inner = Self::substitute_variables(inner, bindings);
                    }
                }
                if let Some(selection) = &mut rewritten.selection {
                    *selection = Self::substitute_variables(selection, bindings);
                }
                if let Some(having) = &mut rewritten.having {
                    *having = Self::substitute_variables(having, bindings);
                }
                match &mut rewritten.group_by {
                    GroupByExpr::All(_) => {}
                    GroupByExpr::Expressions(exprs, _) => {
                        for expr in exprs {
                            *expr = Self::substitute_variables(expr, bindings);
                        }
                    }
                }
                SetExpr::Select(Box::new(rewritten))
            }
            SetExpr::Values(values) => {
                let mut rewritten = values.clone();
                for row in &mut rewritten.rows {
                    for expr in &mut row.content {
                        *expr = Self::substitute_variables(expr, bindings);
                    }
                }
                SetExpr::Values(rewritten)
            }
            SetExpr::SetOperation { op, set_quantifier, left, right } => {
                SetExpr::SetOperation {
                    op: *op,
                    set_quantifier: *set_quantifier,
                    left: Box::new(Self::substitute_variables_in_set_expr(left, bindings)),
                    right: Box::new(Self::substitute_variables_in_set_expr(right, bindings)),
                }
            }
            SetExpr::Query(query) => {
                SetExpr::Query(Box::new(Self::substitute_variables_in_query(query, bindings)))
            }
            other => other.clone(),
        }
    }

    /// Translates `PostgreSQL` UUID function names to the configured `SQLite`
    /// function name.
    ///
    /// Replaces `gen_random_uuid()`, `uuidv4()`, and `uuidv7()` with the
    /// configured UUID function name from options.
    fn translate_uuid_function(expr_str: &str, options: &Pg2SqliteOptions) -> String {
        use crate::traits::TranslationOptions;
        if let Ok(mut parsed_expr) = Self::parse_expression(expr_str) {
            let mut context = PlPgSqlContext::new();
            Self::transform_expr(&mut parsed_expr, &mut context, options);
            return parsed_expr.to_string();
        }

        let target_func = options.get_uuid_function_name();

        // Replace common PostgreSQL UUID function names
        expr_str
            .replace("gen_random_uuid()", &format!("{target_func}()"))
            .replace("GEN_RANDOM_UUID()", &format!("{target_func}()"))
            .replace("uuid_generate_v4()", &format!("{target_func}()"))
            .replace("UUID_GENERATE_V4()", &format!("{target_func}()"))
            .replace("uuidv4()", &format!("{target_func}()"))
            .replace("UUIDV4()", &format!("{target_func}()"))
            .replace("uuidv7()", &format!("{target_func}()"))
            .replace("UUIDV7()", &format!("{target_func}()"))
    }

    /// Parses an expression string into an Expr AST.
    fn parse_expression(expr_str: &str) -> Result<Expr, Error> {
        let dialect = sqlparser::dialect::PostgreSqlDialect {};
        let mut parser =
            sqlparser::parser::Parser::new(&dialect).try_with_sql(expr_str).map_err(|e| {
                Error::UnknownPostgresFeature(format!("Failed to parse expression: {e}"))
            })?;

        parser
            .parse_expr()
            .map_err(|e| Error::UnknownPostgresFeature(format!("Failed to parse expression: {e}")))
    }

    /// Injects a condition into a statement's WHERE clause.
    fn inject_condition_into_statement(stmt: &mut Statement, condition: &str) {
        let Ok(cond_expr) = Self::parse_expression(condition) else {
            return;
        };

        let _ = inject_condition_into_dml_statement(stmt, cond_expr);
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{Expr, Query, SetExpr, Statement, TableFactor},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::PlPgSqlTranslator;
    use crate::{
        impls::translator_impls::plpgsql::context::{PlPgSqlContext, VariableBinding},
        prelude::Pg2SqliteOptions,
        traits::TranslationOptions,
    };

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap()
    }

    fn parse_statement(sql: &str) -> Statement {
        Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap().remove(0)
    }

    fn parse_query(sql: &str) -> Query {
        match parse_statement(sql) {
            Statement::Query(query) => *query,
            other => panic!("expected query, got: {other:?}"),
        }
    }

    fn parse_expr(expr: &str) -> Expr {
        Parser::new(&PostgreSqlDialect {}).try_with_sql(expr).unwrap().parse_expr().unwrap()
    }

    #[test]
    fn uuid_function_translation_and_expression_parsing_behave_as_expected() {
        let options = Pg2SqliteOptions::default().with_uuid_function_name("uuid7".to_string());
        let translated = PlPgSqlTranslator::translate_uuid_function(
            "gen_random_uuid() + uuidv4() + uuidv7()",
            &options,
        );
        assert_eq!(translated, "uuid7() + uuid7() + uuid7()");

        let qualified = PlPgSqlTranslator::translate_uuid_function(
            "public.gen_random_uuid() + pg_catalog.uuid_generate_v4()",
            &options,
        );
        assert_eq!(qualified, "uuid7() + uuid7()");

        let parsed = PlPgSqlTranslator::parse_expression("1 + 2").unwrap();
        assert_eq!(parsed.to_string(), "1 + 2");

        let err = PlPgSqlTranslator::parse_expression("THIS IS NOT SQL").unwrap_err();
        assert!(err.to_string().contains("Failed to parse expression"));
    }

    #[test]
    fn handle_set_statement_tracks_scoped_and_persistent_bindings() {
        let mut ctx = PlPgSqlContext::new();

        let scoped = parse_statement("SET v_id = 42");
        if let Statement::Set(set) = scoped {
            PlPgSqlTranslator::handle_set_statement(&set, &mut ctx);
        } else {
            panic!("expected SET statement");
        }
        assert_eq!(ctx.get_binding("v_id").map(|b| b.expression.as_str()), Some("42"));

        let persistent = parse_statement("SET v_sub = (SELECT NEW.id)");
        if let Statement::Set(set) = persistent {
            PlPgSqlTranslator::handle_set_statement(&set, &mut ctx);
        } else {
            panic!("expected SET statement");
        }
        assert_eq!(
            ctx.get_binding("v_sub").map(|b| b.expression.as_str()),
            Some("(SELECT NEW.id)")
        );
    }

    #[test]
    fn transform_values_to_select_handles_empty_bindings_and_rejects_multi_row_values() {
        let query = parse_query("VALUES (1)");
        let SetExpr::Values(values) = query.body.as_ref() else {
            panic!("expected values");
        };

        let transformed =
            PlPgSqlTranslator::transform_values_to_select(values, &[], Some("TRUE")).unwrap();
        let SetExpr::Select(select) = transformed else {
            panic!("expected select output");
        };
        assert!(matches!(select.from[0].relation, TableFactor::Derived { .. }));
        let selection = select.selection.as_ref().map(ToString::to_string).unwrap();
        assert!(selection.eq_ignore_ascii_case("true"), "unexpected selection: {selection}");

        let multi = parse_query("VALUES (1), (2)");
        let SetExpr::Values(multi_values) = multi.body.as_ref() else {
            panic!("expected values");
        };
        let err =
            PlPgSqlTranslator::transform_values_to_select(multi_values, &[], None).unwrap_err();
        assert!(err.to_string().contains("Multi-row VALUES in trigger not supported"));
    }

    #[test]
    fn substitute_variables_and_reference_detection_cover_nested_shapes() {
        let bindings =
            vec![VariableBinding { name: "v_id".to_string(), expression: "42".to_string() }];
        let expr = parse_expr("(v_id + 1) * 2");
        let substituted = PlPgSqlTranslator::substitute_variables(&expr, &bindings);
        assert!(substituted.to_string().contains("v_id.val"));

        assert!(PlPgSqlTranslator::expr_references_variable(&parse_expr("v_id"), "v_id"));
        assert!(PlPgSqlTranslator::expr_references_variable(&parse_expr("t.v_id"), "v_id"));
        assert!(!PlPgSqlTranslator::expr_references_variable(&parse_expr("other"), "v_id"));
    }

    #[test]
    fn expr_references_variable_does_not_match_identifier_substrings() {
        assert!(!PlPgSqlTranslator::expr_references_variable(&parse_expr("other_id + 1"), "id"));
    }

    #[test]
    fn inject_condition_into_statement_updates_where_and_ignores_invalid_conditions() {
        let mut update = parse_statement("UPDATE users SET active = TRUE");
        PlPgSqlTranslator::inject_condition_into_statement(&mut update, "NEW.kind = 'x'");
        assert!(update.to_string().contains("WHERE NEW.kind = 'x'"));

        let mut delete = parse_statement("DELETE FROM users");
        PlPgSqlTranslator::inject_condition_into_statement(&mut delete, "NEW.kind = 'x'");
        assert!(delete.to_string().contains("WHERE NEW.kind = 'x'"));

        let mut unchanged = parse_statement("DELETE FROM users");
        let before = unchanged.to_string();
        PlPgSqlTranslator::inject_condition_into_statement(&mut unchanged, "NOT (");
        assert_eq!(unchanged.to_string(), before);
    }

    #[test]
    fn inject_condition_into_statement_injects_insert_select_and_skips_insert_values() {
        let mut insert_select = parse_statement("INSERT INTO users(id) SELECT id FROM users");
        PlPgSqlTranslator::inject_condition_into_statement(&mut insert_select, "NEW.kind = 'x'");
        assert!(
            insert_select.to_string().contains("WHERE NEW.kind = 'x'"),
            "INSERT ... SELECT should receive IF condition injection"
        );

        let mut insert_values = parse_statement("INSERT INTO users(id) VALUES (1)");
        let before = insert_values.to_string();
        PlPgSqlTranslator::inject_condition_into_statement(&mut insert_values, "NEW.kind = 'x'");
        assert_eq!(
            insert_values.to_string(),
            before,
            "INSERT ... VALUES should remain unchanged when condition injection is unsupported"
        );
    }

    #[test]
    fn transform_with_insert_to_subquery_rewrites_on_conflict_do_nothing() {
        let query = parse_query(
            r#"
            WITH RECURSIVE cte AS (SELECT 1 AS id)
            INSERT INTO users (id)
            SELECT id FROM cte
            ON CONFLICT (id) DO NOTHING
            "#,
        );

        let with = query.with.as_ref().unwrap().clone();
        let SetExpr::Insert(Statement::Insert(insert)) = query.body.as_ref() else {
            panic!("expected set-expr insert");
        };

        let mut ctx = PlPgSqlContext::new();
        let options = Pg2SqliteOptions::default();
        let statements =
            PlPgSqlTranslator::transform_with_insert_to_subquery(&with, insert, &mut ctx, &options)
                .unwrap();
        let translated = match &statements[0] {
            Statement::Insert(insert) => insert,
            other => panic!("expected insert statement, got: {other:?}"),
        };
        assert!(translated.or.is_some(), "expected INSERT OR IGNORE rewrite");
        assert!(translated.on.is_none(), "expected ON CONFLICT to be removed");
    }

    #[test]
    fn transform_query_body_covers_set_operation_path() {
        let query = parse_query("SELECT 1 UNION ALL SELECT 2");
        let mut ctx = PlPgSqlContext::new();
        let options = Pg2SqliteOptions::default();

        let transformed =
            PlPgSqlTranslator::transform_query_body(query.body.as_ref(), &mut ctx, &options)
                .unwrap();
        assert!(matches!(transformed, SetExpr::SetOperation { .. }));
    }

    #[test]
    fn query_reference_detection_and_with_delete_transform_cover_branches() {
        let query = parse_query("SELECT id FROM cte");
        assert!(PlPgSqlTranslator::query_references_ctes(&query, &["cte".to_string()]));
        assert!(!PlPgSqlTranslator::query_references_ctes(&query, &["other".to_string()]));

        let with_delete = parse_query(
            r#"
            WITH RECURSIVE ids AS (SELECT 1 AS id)
            DELETE FROM users
            WHERE id IN (SELECT id FROM ids)
            "#,
        );
        let with = with_delete.with.as_ref().unwrap().clone();
        let SetExpr::Delete(Statement::Delete(delete)) = with_delete.body.as_ref() else {
            panic!("expected set-expr delete");
        };

        let mut ctx = PlPgSqlContext::new();
        let options = Pg2SqliteOptions::default();
        let transformed =
            PlPgSqlTranslator::transform_with_delete_to_subquery(&with, delete, &mut ctx, &options);
        assert_eq!(transformed.len(), 1);
        assert!(matches!(transformed[0], Statement::Delete(_)));
    }

    #[test]
    fn translate_insert_statement_uses_fast_path_without_bindings() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let insert_stmt = parse_statement("INSERT INTO users(id) VALUES (1)");
        let Statement::Insert(insert) = insert_stmt else {
            panic!("expected insert");
        };
        let mut ctx = PlPgSqlContext::new();

        let translated =
            PlPgSqlTranslator::translate_insert_statement(&insert, &mut ctx, &schema, &options)
                .unwrap();
        assert!(!translated.is_empty());
    }

    #[test]
    fn translate_query_statement_falls_back_for_non_insert_delete_with_bodies() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let mut ctx = PlPgSqlContext::new();

        let query = parse_query(
            r#"
            WITH cte AS (SELECT 1 AS id)
            SELECT id FROM cte WHERE id IN (SELECT id FROM cte)
            "#,
        );

        let out = PlPgSqlTranslator::translate_query_statement(&query, &mut ctx, &schema, &options)
            .unwrap();
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Statement::Query(_)));
    }

    #[test]
    fn transform_with_insert_to_subquery_rejects_source_less_insert() {
        let query = parse_query("WITH cte AS (SELECT 1 AS id) SELECT id FROM cte");
        let with = query.with.as_ref().unwrap().clone();
        let Statement::Insert(mut insert) = parse_statement("INSERT INTO users(id) VALUES (1)")
        else {
            panic!("expected insert");
        };
        insert.source = None;

        let mut ctx = PlPgSqlContext::new();
        let options = Pg2SqliteOptions::default();
        let err = PlPgSqlTranslator::transform_with_insert_to_subquery(
            &with, &insert, &mut ctx, &options,
        )
        .unwrap_err();
        assert!(err.to_string().contains("without source SELECT"));
    }

    #[test]
    fn transform_with_insert_to_subquery_handles_projection_shapes() {
        let options = Pg2SqliteOptions::default();
        let mut ctx = PlPgSqlContext::new();

        let q1 = parse_query(
            r#"
            WITH cte AS (SELECT 1 AS id)
            INSERT INTO users (id) SELECT id FROM cte
            "#,
        );
        let with1 = q1.with.as_ref().unwrap().clone();
        let SetExpr::Insert(Statement::Insert(insert1)) = q1.body.as_ref() else {
            panic!("expected set-expr insert");
        };
        let s1 = PlPgSqlTranslator::transform_with_insert_to_subquery(
            &with1, insert1, &mut ctx, &options,
        )
        .unwrap();
        assert!(s1[0].to_string().contains("recursive_cte_subquery.id"));

        let q2 = parse_query(
            r#"
            WITH cte AS (SELECT 1 AS id)
            INSERT INTO users (id) SELECT cte.id FROM cte
            "#,
        );
        let with2 = q2.with.as_ref().unwrap().clone();
        let SetExpr::Insert(Statement::Insert(insert2)) = q2.body.as_ref() else {
            panic!("expected set-expr insert");
        };
        let s2 = PlPgSqlTranslator::transform_with_insert_to_subquery(
            &with2, insert2, &mut ctx, &options,
        )
        .unwrap();
        assert!(s2[0].to_string().contains("recursive_cte_subquery.id"));

        let q3 = parse_query(
            r#"
            WITH cte AS (SELECT 1 AS id)
            INSERT INTO users (id) SELECT id + 1 FROM cte
            "#,
        );
        let with3 = q3.with.as_ref().unwrap().clone();
        let SetExpr::Insert(Statement::Insert(insert3)) = q3.body.as_ref() else {
            panic!("expected set-expr insert");
        };
        let s3 = PlPgSqlTranslator::transform_with_insert_to_subquery(
            &with3, insert3, &mut ctx, &options,
        )
        .unwrap();
        assert!(s3[0].to_string().contains("SELECT * FROM"), "unexpected SQL: {}", s3[0]);
    }

    #[test]
    fn inject_with_into_subqueries_handles_binary_unary_and_nested_cases() {
        let mut query = parse_query(
            r#"
            WITH ids AS (SELECT 1 AS id)
            DELETE FROM users
            WHERE (id IN (SELECT id FROM ids)) AND (NOT (id IN (SELECT id FROM ids)))
            "#,
        );
        let with = query.with.clone().unwrap();
        let SetExpr::Delete(Statement::Delete(delete)) = query.body.as_mut() else {
            panic!("expected delete");
        };
        let selection = delete.selection.as_mut().unwrap();

        let mut ctx = PlPgSqlContext::new();
        let options = Pg2SqliteOptions::default();
        PlPgSqlTranslator::inject_with_into_in_subqueries(selection, &with, &mut ctx, &options);

        let selection_sql = selection.to_string();
        assert!(selection_sql.contains("WITH ids AS"), "unexpected SQL: {selection_sql}");
    }

    #[test]
    fn transform_query_body_and_set_expr_cover_insert_delete_select_and_other_paths() {
        let mut ctx = PlPgSqlContext::new();
        let options = Pg2SqliteOptions::default();

        let Statement::Insert(insert) = parse_statement("INSERT INTO users(id) VALUES (1)") else {
            panic!("expected insert");
        };
        let insert_set = SetExpr::Insert(Statement::Insert(insert));
        assert!(matches!(
            PlPgSqlTranslator::transform_query_body(&insert_set, &mut ctx, &options).unwrap(),
            SetExpr::Insert(_)
        ));

        let Statement::Delete(delete) = parse_statement("DELETE FROM users WHERE id IN (SELECT 1)")
        else {
            panic!("expected delete");
        };
        let delete_set = SetExpr::Delete(Statement::Delete(delete));
        assert!(matches!(
            PlPgSqlTranslator::transform_query_body(&delete_set, &mut ctx, &options).unwrap(),
            SetExpr::Delete(_)
        ));

        let select_query = parse_query("SELECT gen_random_uuid(), NEW.id FROM users");
        assert!(matches!(
            PlPgSqlTranslator::transform_query_body(select_query.body.as_ref(), &mut ctx, &options)
                .unwrap(),
            SetExpr::Select(_)
        ));

        let values_query = parse_query("VALUES (1)");
        assert!(matches!(
            PlPgSqlTranslator::transform_query_body(values_query.body.as_ref(), &mut ctx, &options)
                .unwrap(),
            SetExpr::Values(_)
        ));

        let mut set_expr = values_query.body.as_ref().clone();
        PlPgSqlTranslator::transform_set_expr(&mut set_expr, &mut ctx, &options);
        assert!(matches!(set_expr, SetExpr::Values(_)));
    }

    #[test]
    fn transform_table_with_joins_and_expr_cover_recursive_paths() {
        let mut ctx = PlPgSqlContext::new();
        let options = Pg2SqliteOptions::default();

        let query = parse_query(
            "SELECT gen_random_uuid(), x FROM users u LEFT JOIN teams t ON u.team_id = t.id",
        );
        let SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected select");
        };
        let mut select = (**select).clone();
        for from in &mut select.from {
            PlPgSqlTranslator::transform_table_with_joins(from, &mut ctx, &options);
        }
        PlPgSqlTranslator::transform_selection(&mut select.selection, &mut ctx, &options);
        for item in &mut select.projection {
            if let sqlparser::ast::SelectItem::UnnamedExpr(expr) = item {
                PlPgSqlTranslator::transform_expr(expr, &mut ctx, &options);
            }
        }
        let projection_sql = select.projection[0].to_string();
        assert!(projection_sql.contains("uuid"), "unexpected projection: {projection_sql}");

        let mut in_subquery = parse_expr("x IN (SELECT y FROM t)");
        PlPgSqlTranslator::transform_expr(&mut in_subquery, &mut ctx, &options);
        assert!(in_subquery.to_string().contains("SELECT y FROM t"));
    }

    #[test]
    fn translate_insert_statement_covers_uuid_tracking_and_select_source_paths() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(
                &PostgreSqlDialect {},
                "CREATE TABLE users(id TEXT, name TEXT); CREATE TABLE audit(id TEXT);",
            )
            .unwrap(),
            "test".to_string(),
        )
        .unwrap();
        let options = Pg2SqliteOptions::default();

        let mut ctx = PlPgSqlContext::new();
        ctx.add_binding(VariableBinding {
            name: "v_id".to_string(),
            expression: "uuidv7()".to_string(),
        });

        let Statement::Insert(insert_values) =
            parse_statement("INSERT INTO users (id, name) VALUES (v_id, 'a')")
        else {
            panic!("expected insert");
        };
        let translated_values = PlPgSqlTranslator::translate_insert_statement(
            &insert_values,
            &mut ctx,
            &schema,
            &options,
        )
        .unwrap();
        assert!(!translated_values.is_empty());
        assert!(ctx.get_uuid_first_use("v_id").is_some());

        let Statement::Insert(insert_select) =
            parse_statement("INSERT INTO users (id) SELECT v_id")
        else {
            panic!("expected insert");
        };
        ctx.push_condition("NEW.kind = 'a'".to_string());
        let translated_select = PlPgSqlTranslator::translate_insert_statement(
            &insert_select,
            &mut ctx,
            &schema,
            &options,
        )
        .unwrap();
        assert!(!translated_select.is_empty());

        let mut table_fn_insert = insert_values.clone();
        let Expr::Function(func) = parse_expr("remote()") else {
            panic!("expected function expression");
        };
        table_fn_insert.table = sqlparser::ast::TableObject::TableFunction(func);
        let err = PlPgSqlTranslator::translate_insert_statement(
            &table_fn_insert,
            &mut ctx,
            &schema,
            &options,
        )
        .unwrap_err();
        assert!(err.to_string().contains("table function not supported"));
    }

    #[test]
    fn translate_insert_statement_tracks_schema_qualified_uuid_generate_v4_bindings() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, "CREATE TABLE users(id TEXT, name TEXT);")
                .unwrap(),
            "test".to_string(),
        )
        .unwrap();
        let options = Pg2SqliteOptions::default();
        let mut ctx = PlPgSqlContext::new();
        ctx.add_binding(VariableBinding {
            name: "v_id".to_string(),
            expression: "pg_catalog.uuid_generate_v4()".to_string(),
        });

        let Statement::Insert(insert_values) =
            parse_statement("INSERT INTO users (id, name) VALUES (v_id, 'a')")
        else {
            panic!("expected insert");
        };

        let translated = PlPgSqlTranslator::translate_insert_statement(
            &insert_values,
            &mut ctx,
            &schema,
            &options,
        )
        .expect("values insert should translate");
        assert!(!translated.is_empty());
        assert!(
            ctx.get_uuid_first_use("v_id").is_some(),
            "schema-qualified uuid_generate_v4() should be tracked for last_insert_rowid pattern"
        );
    }

    #[test]
    fn translate_statement_other_variant_and_inject_noop_paths_are_covered() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let mut ctx = PlPgSqlContext::new();

        let drop_stmt = parse_statement("DROP TABLE IF EXISTS users");
        let translated =
            PlPgSqlTranslator::translate_statement(&drop_stmt, &mut ctx, &schema, &options)
                .expect("fallback statement translation should work");
        assert_eq!(translated.len(), 1);

        let mut insert = parse_statement("INSERT INTO users(id) VALUES (1)");
        let before = insert.to_string();
        PlPgSqlTranslator::inject_condition_into_statement(&mut insert, "id > 0");
        assert_eq!(insert.to_string(), before);
    }

    #[test]
    fn transform_set_expr_and_transform_expr_cover_remaining_select_and_default_paths() {
        let options = Pg2SqliteOptions::default();
        let mut ctx = PlPgSqlContext::new();

        let query = parse_query(
            "SELECT u.id AS uid FROM users u RIGHT JOIN teams t ON u.team_id = t.id \
             WHERE NOT (u.id IN (1, 2))",
        );
        let mut set_expr = query.body.as_ref().clone();
        PlPgSqlTranslator::transform_set_expr(&mut set_expr, &mut ctx, &options);
        let SetExpr::Select(select) = set_expr else {
            panic!("expected transformed select");
        };
        assert!(select.selection.is_some());
        assert_eq!(select.projection.len(), 1);

        let mut in_subquery = parse_expr("id IN (SELECT id FROM teams)");
        PlPgSqlTranslator::transform_expr(&mut in_subquery, &mut ctx, &options);
        assert!(in_subquery.to_string().contains("SELECT id FROM teams"));

        let mut scalar_subquery = parse_expr("(SELECT id FROM teams)");
        PlPgSqlTranslator::transform_expr(&mut scalar_subquery, &mut ctx, &options);
        assert!(scalar_subquery.to_string().contains("SELECT id FROM teams"));

        let mut unary = parse_expr("NOT flag");
        PlPgSqlTranslator::transform_expr(&mut unary, &mut ctx, &options);
        assert!(unary.to_string().contains("NOT"));

        let mut table_set_expr = SetExpr::Table(Box::new(sqlparser::ast::Table {
            table_name: Some("users".to_string()),
            schema_name: None,
        }));
        PlPgSqlTranslator::transform_set_expr(&mut table_set_expr, &mut ctx, &options);
        assert!(matches!(table_set_expr, SetExpr::Table(_)));
    }

    #[test]
    fn transform_subquery_matches_cte_query_for_limit_expression() {
        let options =
            Pg2SqliteOptions::default().with_uuid_function_name("sqlite_uuid".to_string());
        let mut cte_ctx = PlPgSqlContext::new();
        let mut subquery_ctx = PlPgSqlContext::new();

        let query =
            parse_query("SELECT id FROM teams ORDER BY id LIMIT gen_random_uuid() + uuidv4()");
        let mut cte_query = query.clone();
        let mut subquery = query;

        PlPgSqlTranslator::transform_cte_query(&mut cte_query, &mut cte_ctx, &options);
        PlPgSqlTranslator::transform_subquery(&mut subquery, &mut subquery_ctx, &options);

        assert_eq!(
            subquery.to_string(),
            cte_query.to_string(),
            "subquery transform should stay in parity with cte query transform"
        );
        assert!(
            subquery.to_string().contains("sqlite_uuid()"),
            "LIMIT expressions inside subqueries should be transformed: {subquery}"
        );
    }

    #[test]
    fn transform_subquery_matches_cte_query_for_offset_comma_limit_expression() {
        let options =
            Pg2SqliteOptions::default().with_uuid_function_name("sqlite_uuid".to_string());
        let mut cte_ctx = PlPgSqlContext::new();
        let mut subquery_ctx = PlPgSqlContext::new();

        let mut query = parse_query("SELECT id FROM teams ORDER BY id");
        query.limit_clause = Some(sqlparser::ast::LimitClause::OffsetCommaLimit {
            offset: parse_expr("gen_random_uuid()"),
            limit: parse_expr("uuidv4()"),
        });
        let mut cte_query = query.clone();
        let mut subquery = query;

        PlPgSqlTranslator::transform_cte_query(&mut cte_query, &mut cte_ctx, &options);
        PlPgSqlTranslator::transform_subquery(&mut subquery, &mut subquery_ctx, &options);

        assert_eq!(
            subquery.to_string(),
            cte_query.to_string(),
            "subquery transform should stay in parity with cte query transform"
        );
        let sql = subquery.to_string();
        assert!(
            sql.contains("sqlite_uuid()"),
            "OffsetCommaLimit expressions inside subqueries should be transformed: {sql}"
        );
    }

    #[test]
    fn transform_subquery_matches_cte_query_for_fetch_expression() {
        let options =
            Pg2SqliteOptions::default().with_uuid_function_name("sqlite_uuid".to_string());
        let mut cte_ctx = PlPgSqlContext::new();
        let mut subquery_ctx = PlPgSqlContext::new();

        let mut query = parse_query("SELECT id FROM teams ORDER BY id");
        query.fetch = Some(sqlparser::ast::Fetch {
            with_ties: false,
            percent: false,
            quantity: Some(parse_expr("gen_random_uuid() + uuidv4()")),
        });
        let mut cte_query = query.clone();
        let mut subquery = query;

        PlPgSqlTranslator::transform_cte_query(&mut cte_query, &mut cte_ctx, &options);
        PlPgSqlTranslator::transform_subquery(&mut subquery, &mut subquery_ctx, &options);

        assert_eq!(
            subquery.to_string(),
            cte_query.to_string(),
            "subquery transform should stay in parity with cte query transform"
        );
        assert!(
            subquery.to_string().contains("sqlite_uuid()"),
            "FETCH quantity inside subqueries should be transformed: {subquery}"
        );
    }

    #[test]
    fn transform_delete_and_insert_statement_cover_function_name_and_non_select_source_paths() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let mut ctx = PlPgSqlContext::new();
        ctx.add_binding(VariableBinding { name: "v_id".to_string(), expression: "1".to_string() });

        let Statement::Delete(mut delete_stmt) = parse_statement("DELETE FROM users WHERE id = 1")
        else {
            panic!("expected delete");
        };
        let sqlparser::ast::FromTable::WithFromKeyword(tables) = delete_stmt.from.clone() else {
            panic!("expected WITH FROM variant");
        };
        delete_stmt.from = sqlparser::ast::FromTable::WithoutKeyword(tables);
        let transformed_delete =
            PlPgSqlTranslator::transform_delete_for_sqlite(&delete_stmt, &mut ctx, &options);
        assert!(matches!(transformed_delete.from, sqlparser::ast::FromTable::WithoutKeyword(_)));

        let Statement::Insert(mut insert_fn_table) =
            parse_statement("INSERT INTO users(id) VALUES (v_id)")
        else {
            panic!("expected insert");
        };
        insert_fn_table.table =
            sqlparser::ast::TableObject::TableName(sqlparser::ast::ObjectName(vec![
                sqlparser::ast::ObjectNamePart::Function(sqlparser::ast::ObjectNamePartFunction {
                    name: sqlparser::ast::Ident::new("remote"),
                    args: vec![],
                }),
            ]));
        let translated = PlPgSqlTranslator::translate_insert_statement(
            &insert_fn_table,
            &mut ctx,
            &schema,
            &options,
        )
        .expect("function-style table object should still translate");
        assert!(!translated.is_empty());

        let Statement::Insert(mut insert_table_source) =
            parse_statement("INSERT INTO users(id) VALUES (v_id)")
        else {
            panic!("expected insert");
        };
        insert_table_source.source = Some(Box::new(Query {
            with: None,
            body: Box::new(SetExpr::Table(Box::new(sqlparser::ast::Table {
                table_name: Some("users".to_string()),
                schema_name: None,
            }))),
            order_by: None,
            limit_clause: None,
            fetch: None,
            locks: vec![],
            for_clause: None,
            settings: None,
            format_clause: None,
            pipe_operators: vec![],
        }));
        let result = PlPgSqlTranslator::translate_insert_statement(
            &insert_table_source,
            &mut ctx,
            &schema,
            &options,
        );
        assert!(result.is_err(), "TABLE expression in INSERT source should error");
    }

    #[test]
    fn translate_insert_statement_merges_condition_with_existing_selection() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let mut ctx = PlPgSqlContext::new();
        ctx.add_binding(VariableBinding { name: "v_id".to_string(), expression: "1".to_string() });
        ctx.push_condition("NEW.kind = 'a'".to_string());

        let Statement::Insert(insert_stmt) =
            parse_statement("INSERT INTO users(id) SELECT v_id WHERE id > 0")
        else {
            panic!("expected insert");
        };
        let translated = PlPgSqlTranslator::translate_insert_statement(
            &insert_stmt,
            &mut ctx,
            &schema,
            &options,
        )
        .expect("insert-select should translate");
        let sql = translated[0].to_string();
        assert!(sql.contains("id > 0"));
        assert!(sql.contains("NEW.kind = 'a'"));
    }

    #[test]
    fn substitute_variables_leaves_unbound_identifiers_and_handles_unary_ops() {
        let bindings =
            vec![VariableBinding { name: "v_id".to_string(), expression: "1".to_string() }];
        let expr = parse_expr("-other");
        let substituted = PlPgSqlTranslator::substitute_variables(&expr, &bindings);
        assert_eq!(substituted.to_string(), "-other");
    }

    #[test]
    fn substitute_variables_rewrites_function_arguments() {
        let bindings =
            vec![VariableBinding { name: "v_id".to_string(), expression: "1".to_string() }];
        let expr = parse_expr("COALESCE(v_id, 0)");
        let substituted = PlPgSqlTranslator::substitute_variables(&expr, &bindings);
        assert!(
            substituted.to_string().contains("v_id.val"),
            "Expected bound variable inside function args to be rewritten: {substituted}"
        );
    }

    #[test]
    fn substitute_variables_rewrites_case_expressions() {
        let bindings =
            vec![VariableBinding { name: "v_id".to_string(), expression: "1".to_string() }];
        let expr = parse_expr("CASE WHEN v_id > 0 THEN v_id ELSE 0 END");
        let substituted = PlPgSqlTranslator::substitute_variables(&expr, &bindings);
        assert!(
            substituted.to_string().contains("v_id.val"),
            "Expected bound variable inside CASE to be rewritten: {substituted}"
        );
    }

    #[test]
    fn substitute_variables_rewrites_subquery_projections() {
        let bindings =
            vec![VariableBinding { name: "v_id".to_string(), expression: "1".to_string() }];
        let expr = parse_expr("(SELECT v_id)");
        let substituted = PlPgSqlTranslator::substitute_variables(&expr, &bindings);
        assert!(
            substituted.to_string().contains("v_id.val"),
            "Expected bound variable inside subquery projection to be rewritten: {substituted}"
        );
    }

    #[test]
    fn substitute_variables_rewrites_tuple_items() {
        let bindings =
            vec![VariableBinding { name: "v_id".to_string(), expression: "1".to_string() }];
        let expr = parse_expr("(v_id, 1)");
        let substituted = PlPgSqlTranslator::substitute_variables(&expr, &bindings);
        assert!(
            substituted.to_string().contains("v_id.val"),
            "Expected bound variable inside tuple to be rewritten: {substituted}"
        );
    }

    #[test]
    fn substitute_variables_rewrites_array_items() {
        let bindings =
            vec![VariableBinding { name: "v_id".to_string(), expression: "1".to_string() }];
        let expr = parse_expr("ARRAY[v_id, 1]");
        let substituted = PlPgSqlTranslator::substitute_variables(&expr, &bindings);
        assert!(
            substituted.to_string().contains("v_id.val"),
            "Expected bound variable inside array literal to be rewritten: {substituted}"
        );
    }

    #[test]
    fn substitute_variables_rewrites_exists_subqueries() {
        let bindings =
            vec![VariableBinding { name: "v_id".to_string(), expression: "1".to_string() }];
        let expr = parse_expr("EXISTS (SELECT v_id)");
        let substituted = PlPgSqlTranslator::substitute_variables(&expr, &bindings);
        assert!(
            substituted.to_string().contains("v_id.val"),
            "Expected bound variable inside EXISTS subquery to be rewritten: {substituted}"
        );
    }

    #[test]
    fn substitute_variables_rewrites_subquery_order_by_expressions() {
        let bindings =
            vec![VariableBinding { name: "v_id".to_string(), expression: "1".to_string() }];
        let expr = parse_expr("(SELECT 1 ORDER BY v_id)");
        let substituted = PlPgSqlTranslator::substitute_variables(&expr, &bindings);
        assert!(
            substituted.to_string().contains("v_id.val"),
            "Expected bound variable inside subquery ORDER BY to be rewritten: {substituted}"
        );
    }

    #[test]
    fn transform_with_insert_to_subquery_covers_values_and_projection_alias_edges() {
        let options = Pg2SqliteOptions::default();
        let mut ctx = PlPgSqlContext::new();

        let values_query =
            parse_query("WITH cte AS (SELECT 1 AS id) INSERT INTO users(id) VALUES (1)");
        let with_values = values_query.with.as_ref().expect("with should exist").clone();
        let SetExpr::Insert(Statement::Insert(values_insert)) = values_query.body.as_ref() else {
            panic!("expected set-expr insert");
        };
        let transformed = PlPgSqlTranslator::transform_with_insert_to_subquery(
            &with_values,
            values_insert,
            &mut ctx,
            &options,
        )
        .expect("values source should be rewritten");
        assert!(transformed[0].to_string().contains("SELECT * FROM"));

        let alias_query = parse_query(
            "WITH cte AS (SELECT 1 AS id) INSERT INTO users(id) SELECT id AS out_id FROM cte",
        );
        let with_alias = alias_query.with.as_ref().expect("with should exist").clone();
        let SetExpr::Insert(Statement::Insert(mut alias_insert)) =
            alias_query.body.as_ref().clone()
        else {
            panic!("expected set-expr insert");
        };
        if let Some(source) = &mut alias_insert.source
            && let SetExpr::Select(select) = source.body.as_mut()
        {
            select.projection.push(sqlparser::ast::SelectItem::UnnamedExpr(
                Expr::CompoundIdentifier(Vec::new()),
            ));
        }
        let transformed = PlPgSqlTranslator::transform_with_insert_to_subquery(
            &with_alias,
            &alias_insert,
            &mut ctx,
            &options,
        )
        .expect("alias/empty-compound projection should rewrite");
        assert!(!transformed.is_empty());
    }

    #[test]
    fn transform_table_with_joins_handles_inner_left_right_and_full_outer_join_variants() {
        let options = Pg2SqliteOptions::default();
        let mut ctx = PlPgSqlContext::new();

        let inner_query =
            parse_query("SELECT u.id FROM users u INNER JOIN teams t ON u.team_id = t.id");
        let SetExpr::Select(inner_select) = inner_query.body.as_ref() else {
            panic!("expected select");
        };
        let mut inner_from = inner_select.from[0].clone();
        PlPgSqlTranslator::transform_table_with_joins(&mut inner_from, &mut ctx, &options);

        let left_query =
            parse_query("SELECT u.id FROM users u LEFT OUTER JOIN teams t ON u.team_id = t.id");
        let SetExpr::Select(left_select) = left_query.body.as_ref() else {
            panic!("expected select");
        };
        let mut left_from = left_select.from[0].clone();
        PlPgSqlTranslator::transform_table_with_joins(&mut left_from, &mut ctx, &options);

        let right_query =
            parse_query("SELECT u.id FROM users u RIGHT OUTER JOIN teams t ON u.team_id = t.id");
        let SetExpr::Select(right_select) = right_query.body.as_ref() else {
            panic!("expected select");
        };
        let mut right_from = right_select.from[0].clone();
        PlPgSqlTranslator::transform_table_with_joins(&mut right_from, &mut ctx, &options);

        let full_query =
            parse_query("SELECT u.id FROM users u FULL OUTER JOIN teams t ON u.team_id = t.id");
        let SetExpr::Select(full_select) = full_query.body.as_ref() else {
            panic!("expected select");
        };
        let mut full_from = full_select.from[0].clone();
        PlPgSqlTranslator::transform_table_with_joins(&mut full_from, &mut ctx, &options);
    }

    #[test]
    fn parse_expression_reports_both_parser_entry_and_expr_errors() {
        let entry_err = PlPgSqlTranslator::parse_expression("'unterminated").unwrap_err();
        assert!(entry_err.to_string().contains("Failed to parse expression"));

        let expr_err = PlPgSqlTranslator::parse_expression("1 +").unwrap_err();
        assert!(expr_err.to_string().contains("Failed to parse expression"));
    }
}
