//! Main translator for PL/pgSQL to `SQLite`.
//!
//! This module orchestrates the translation of PL/pgSQL function bodies
//! to SQLite-compatible statements.

use sql_traits::structs::ParserDB;
use sqlparser::{
    ast::{
        BeginEndStatements, BinaryOperator, Expr, GroupByExpr, Ident, ObjectName, ObjectNamePart,
        Query, Select, SelectFlavor, SelectItem, Set, SetExpr, Statement, TableAlias, TableFactor,
        TableWithJoins, Value, ValueWithSpan, helpers::attached_token::AttachedToken,
    },
    tokenizer::Span,
};

use super::{
    context::{PlPgSqlContext, VariableBinding},
    cte_builder::CteBuilder,
};
use crate::{errors::Error, options::Pg2SqliteOptions, traits::Translator};

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
        let mut result = Vec::new();
        let mut context = PlPgSqlContext::new();

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

    /// Translates an IF statement.
    fn translate_if_statement(
        if_stmt: &sqlparser::ast::IfStatement,
        context: &mut PlPgSqlContext,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Vec<Statement>, Error> {
        let mut result = Vec::new();

        // Get the condition
        let condition = if_stmt
            .if_block
            .condition
            .as_ref()
            .map_or_else(|| "TRUE".to_string(), ToString::to_string);

        // Push condition onto stack
        context.push_condition(condition);

        // Clear scoped bindings for this IF block (persistent bindings are kept)
        context.clear_scoped_bindings();

        // Clear UUID first-use tracking for this IF block (new UUID variables may be
        // assigned)
        context.clear_uuid_first_use();

        // Translate statements within the IF block
        for stmt in if_stmt.if_block.statements() {
            let translated = Self::translate_statement(stmt, context, schema, options)?;
            result.extend(translated);
        }

        // Pop condition from stack
        context.pop_condition();

        // TODO: Handle ELSE and ELSIF blocks

        Ok(result)
    }

    /// Translates an INSERT statement with CTE injection for variables.
    ///
    /// Uses the `last_insert_rowid()` pattern for UUID variables:
    /// - First INSERT using a UUID variable: uses the expression directly
    ///   (e.g., `uuidv7()`)
    /// - Subsequent INSERTs: use `SELECT col FROM table WHERE rowid =
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
            TableObject::TableFunction(_) => {
                return Err(Error::UnsupportedSQLiteFeature(
                    "INSERT into table function not supported".to_string(),
                ));
            }
        };

        // Get column names from INSERT
        let column_names: Vec<String> = insert.columns.iter().map(|c| c.value.clone()).collect();

        // Process bindings - check which UUID variables need the last_insert_rowid()
        // pattern
        let mut modified_bindings = Vec::new();
        let mut uuid_var_to_column: Vec<(String, String)> = Vec::new();

        for binding in &bindings {
            let is_uuid_gen = {
                let expr_lower = binding.expression.to_lowercase();
                expr_lower.contains("uuidv7()")
                    || expr_lower.contains("uuidv4()")
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
                    // and record it for future INSERTs
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
                        order_by: source.order_by.clone(),
                        limit_clause: source.limit_clause.clone(),
                        fetch: source.fetch.clone(),
                        locks: source.locks.clone(),
                        for_clause: source.for_clause.clone(),
                        settings: source.settings.clone(),
                        format_clause: source.format_clause.clone(),
                        pipe_operators: source.pipe_operators.clone(),
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

    /// Checks if an expression references a specific variable name.
    fn expr_references_variable(expr: &Expr, var_name: &str) -> bool {
        match expr {
            Expr::Identifier(ident) => ident.value == var_name,
            Expr::CompoundIdentifier(idents) => idents.iter().any(|i| i.value == var_name),
            _ => {
                // For now, check string representation
                expr.to_string().contains(var_name)
            }
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
        for expr in row {
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
                    subquery: Box::new(Query {
                        with: None,
                        body: Box::new(SetExpr::Select(Box::new(Select {
                            select_token: AttachedToken::empty(),
                            distinct: None,
                            top: None,
                            top_before_distinct: false,
                            projection: vec![SelectItem::UnnamedExpr(Expr::Value(ValueWithSpan {
                                value: Value::Number("1".to_string(), false),
                                span: Span::empty(),
                            }))],
                            into: None,
                            from: vec![],
                            lateral_views: vec![],
                            selection: None,
                            group_by: GroupByExpr::Expressions(vec![], vec![]),
                            cluster_by: vec![],
                            distribute_by: vec![],
                            sort_by: vec![],
                            having: None,
                            named_window: vec![],
                            qualify: None,
                            window_before_qualify: false,
                            value_table_mode: None,
                            connect_by: None,
                            flavor: SelectFlavor::Standard,
                            exclude: None,
                            optimizer_hint: None,
                            prewhere: None,
                        }))),
                        order_by: None,
                        limit_clause: None,
                        fetch: None,
                        locks: vec![],
                        for_clause: None,
                        settings: None,
                        format_clause: None,
                        pipe_operators: vec![],
                    }),
                    alias: Some(TableAlias {
                        name: Ident::new("_dummy".to_string()),
                        columns: vec![],
                        explicit: false,
                    }),
                },
                joins: vec![],
            });
        }

        // Parse condition if present
        let selection =
            if let Some(cond) = condition { Some(Self::parse_expression(cond)?) } else { None };

        Ok(SetExpr::Select(Box::new(Select {
            select_token: AttachedToken::empty(),
            distinct: None,
            top: None,
            top_before_distinct: false,
            projection: projections,
            into: None,
            from: from_tables,
            lateral_views: vec![],
            selection,
            group_by: GroupByExpr::Expressions(vec![], vec![]),
            cluster_by: vec![],
            distribute_by: vec![],
            sort_by: vec![],
            having: None,
            named_window: vec![],
            qualify: None,
            window_before_qualify: false,
            value_table_mode: None,
            connect_by: None,
            flavor: SelectFlavor::Standard,
            exclude: None,
            optimizer_hint: None,
            prewhere: None,
        })))
    }

    /// Substitutes variable references in an expression with CTE column
    /// references.
    fn substitute_variables(expr: &Expr, bindings: &[VariableBinding]) -> Expr {
        match expr {
            Expr::Identifier(ident) => {
                let name = &ident.value;
                // Check if it's a bound variable
                for binding in bindings {
                    if name == &binding.name {
                        return CteBuilder::variable_reference(name);
                    }
                }
                expr.clone()
            }
            Expr::BinaryOp { left, op, right } => {
                Expr::BinaryOp {
                    left: Box::new(Self::substitute_variables(left, bindings)),
                    op: op.clone(),
                    right: Box::new(Self::substitute_variables(right, bindings)),
                }
            }
            Expr::UnaryOp { op, expr: inner } => {
                Expr::UnaryOp {
                    op: *op,
                    expr: Box::new(Self::substitute_variables(inner, bindings)),
                }
            }
            Expr::Nested(inner) => {
                Expr::Nested(Box::new(Self::substitute_variables(inner, bindings)))
            }
            // For other expression types, return as-is
            _ => expr.clone(),
        }
    }

    /// Translates `PostgreSQL` UUID function names to the configured `SQLite`
    /// function name.
    ///
    /// Replaces `gen_random_uuid()`, `uuidv4()`, and `uuidv7()` with the
    /// configured UUID function name from options.
    fn translate_uuid_function(expr_str: &str, options: &Pg2SqliteOptions) -> String {
        use crate::traits::TranslationOptions;
        let target_func = options.get_uuid_function_name();

        // Replace common PostgreSQL UUID function names
        expr_str
            .replace("gen_random_uuid()", &format!("{target_func}()"))
            .replace("GEN_RANDOM_UUID()", &format!("{target_func}()"))
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

        match stmt {
            Statement::Update(update) => {
                update.selection = match &update.selection {
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
            Statement::Delete(delete) => {
                delete.selection = match &delete.selection {
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
            _ => {}
        }
    }
}
