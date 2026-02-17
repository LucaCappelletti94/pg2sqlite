//! RLS (Row-Level Security) translation from PostgreSQL to SQLite.
//!
//! This module handles the translation of PostgreSQL RLS policies to SQLite
//! by generating:
//! 1. A renamed inner table (e.g., `documents_rls`) containing the actual data
//! 2. A view with the original table name that filters rows based on policies
//! 3. INSTEAD OF triggers on the view for INSERT, UPDATE, DELETE operations

use sql_traits::traits::{ColumnLike, DatabaseLike, PolicyLike, TableLike};
use sqlparser::ast::{
    CreatePolicyCommand, CreateTable, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArgumentList, FunctionArguments, Ident, ObjectName, Statement,
};

use crate::{
    errors::Error,
    traits::{SessionVariablePattern, TranslationOptions},
};

/// Error message used when a row violates RLS policy constraints.
const RLS_VIOLATION_ERROR: &str = "new row violates row-level security policy";

/// Collects all column names from a table.
fn collect_column_names<DB: DatabaseLike>(table: &DB::Table, schema: &DB) -> Vec<String>
where
    DB::Table: TableLike<DB = DB>,
{
    table.columns(schema).map(|c| c.column_name().to_string()).collect()
}

/// Collects primary key column names from a table.
fn collect_pk_column_names<DB: DatabaseLike>(table: &DB::Table, schema: &DB) -> Vec<String>
where
    DB::Table: TableLike<DB = DB>,
{
    table.primary_key_columns(schema).map(|c| c.column_name().to_string()).collect()
}

/// Checks if a table has RLS enabled by looking it up in the schema.
pub fn table_has_rls<DB: DatabaseLike>(table_name: &str, schema: &DB) -> bool
where
    DB::Table: TableLike<DB = DB>,
{
    schema.table(None, table_name).is_some_and(|t| t.has_row_level_security(schema))
}

/// Resolves the correct table name for attaching AFTER triggers.
///
/// When a table has RLS, it's split into a view and a backing table.
/// AFTER triggers must be attached to the backing table (e.g., `table_rls`),
/// not the view, because views don't fire AFTER triggers in SQLite.
///
/// # Arguments
/// * `base_name` - The original table name (e.g., "users")
/// * `table` - The table object from schema
/// * `schema` - The database schema
/// * `options` - Options containing the RLS table suffix
///
/// # Returns
/// The table name to use for trigger attachment:
/// - `table_rls` if table has RLS
/// - `table` if table has no RLS
///
/// # Example
/// ```rust,ignore
/// let trigger_table = resolve_trigger_table_name(
///     "documents",
///     &table_obj,
///     schema,
///     options,
/// );
/// // Returns "documents_rls" if table has RLS, "documents" otherwise
/// ```
pub fn resolve_trigger_table_name<DB: DatabaseLike>(
    base_name: &str,
    table: &DB::Table,
    schema: &DB,
    options: &impl TranslationOptions,
) -> String
where
    DB::Table: TableLike<DB = DB>,
{
    if table.has_row_level_security(schema) {
        let suffix = options.get_rls_table_suffix();
        format!("{base_name}{suffix}")
    } else {
        base_name.to_string()
    }
}

/// Builds a WHERE clause for row identity using primary key columns if
/// available, otherwise falls back to all columns.
fn build_row_identity_clause(columns: &[String], pk_columns: &[String]) -> String {
    let identity_cols = if pk_columns.is_empty() { columns } else { pk_columns };
    identity_cols.iter().map(|c| format!("{c} = OLD.{c}")).collect::<Vec<_>>().join(" AND ")
}

/// Filters policies for a table by the specified commands.
/// Returns policies that match any of the given commands or the All command.
fn filter_policies<'a, DB: DatabaseLike>(
    table: &'a DB::Table,
    schema: &'a DB,
    commands: &[CreatePolicyCommand],
) -> Vec<&'a DB::Policy>
where
    DB::Table: TableLike<DB = DB>,
    DB::Policy: PolicyLike<DB = DB>,
{
    table
        .policies(schema)
        .filter(|p| commands.contains(&p.command()) || p.command() == CreatePolicyCommand::All)
        .collect()
}

/// Context for RLS trigger generation, encapsulating common setup.
struct RlsTriggerContext<'a> {
    table_name: &'a str,
    inner_table_name: String,
}

impl<'a> RlsTriggerContext<'a> {
    fn new<O: TranslationOptions, DB: DatabaseLike>(table: &'a DB::Table, options: &O) -> Self
    where
        DB::Table: TableLike<DB = DB>,
    {
        let table_name = table.table_name();
        let inner_table_name = format!("{}{}", table_name, options.get_rls_table_suffix());
        Self { table_name, inner_table_name }
    }

    fn as_rename_tuple(&self) -> (&str, &str) {
        (self.table_name, self.inner_table_name.as_str())
    }
}

/// Checks if an expression contains a `current_setting('name')` call and
/// returns the setting name if found.
fn find_current_setting_call(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Function(func) => {
            let func_name = func.name.to_string().to_lowercase();
            if func_name == "current_setting"
                && let FunctionArguments::List(FunctionArgumentList { args, .. }) = &func.args
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(value)))) =
                    args.first()
            {
                // Handle both old and new sqlparser Value types
                let value_str = value.to_string();
                // Remove quotes from the string value
                let trimmed = value_str.trim_matches('\'');
                return Some(trimmed.to_string());
            }
            None
        }
        Expr::BinaryOp { left, right, .. } => {
            find_current_setting_call(left).or_else(|| find_current_setting_call(right))
        }
        Expr::UnaryOp { expr, .. } | Expr::Cast { expr, .. } => find_current_setting_call(expr),
        Expr::Nested(inner) | Expr::IsNull(inner) | Expr::IsNotNull(inner) => {
            find_current_setting_call(inner)
        }
        Expr::InList { expr, list, .. } => {
            find_current_setting_call(expr)
                .or_else(|| list.iter().find_map(find_current_setting_call))
        }
        Expr::Between { expr, low, high, .. } => {
            find_current_setting_call(expr)
                .or_else(|| find_current_setting_call(low))
                .or_else(|| find_current_setting_call(high))
        }
        _ => None,
    }
}

/// Checks if an expression contains `current_user`.
fn contains_current_user(expr: &Expr) -> bool {
    match expr {
        Expr::Identifier(ident) => ident.value.to_lowercase() == "current_user",
        // In PostgreSQL, current_user is parsed as a no-argument function
        Expr::Function(func) => func.name.to_string().to_lowercase() == "current_user",
        Expr::BinaryOp { left, right, .. } => {
            contains_current_user(left) || contains_current_user(right)
        }
        Expr::UnaryOp { expr, .. } | Expr::Cast { expr, .. } => contains_current_user(expr),
        Expr::Nested(inner) | Expr::IsNull(inner) | Expr::IsNotNull(inner) => {
            contains_current_user(inner)
        }
        Expr::InList { expr, list, .. } => {
            contains_current_user(expr) || list.iter().any(contains_current_user)
        }
        Expr::Between { expr, low, high, .. } => {
            contains_current_user(expr) || contains_current_user(low) || contains_current_user(high)
        }
        _ => false,
    }
}

/// Validates that all session variable patterns in an expression have mappings
/// configured.
///
/// # Errors
///
/// Returns `Error::SessionVariableMappingNotFound` if a
/// `current_setting('name')` or `current_user` pattern is found in the
/// expression but no corresponding SQLite function mapping is configured.
pub fn validate_session_variables<O: TranslationOptions>(
    expr: &Expr,
    options: &O,
    table_name: &str,
    policy_name: &str,
) -> Result<(), Error> {
    // Check for current_setting calls
    if let Some(setting_name) = find_current_setting_call(expr) {
        let pattern = SessionVariablePattern::CurrentSetting { name: setting_name.clone() };
        if options.find_session_variable_function(&pattern).is_none() {
            return Err(Error::SessionVariableMappingNotFound {
                pattern: format!(
                    "current_setting('{setting_name}') in table '{table_name}', policy '{policy_name}'"
                ),
            });
        }
    }

    // Check for current_user
    if contains_current_user(expr) {
        let pattern = SessionVariablePattern::CurrentUser;
        if options.find_session_variable_function(&pattern).is_none() {
            return Err(Error::SessionVariableMappingNotFound {
                pattern: format!("current_user in table '{table_name}', policy '{policy_name}'"),
            });
        }
    }

    Ok(())
}

/// Validates that all policies for a table have required session variable
/// mappings configured.
///
/// # Errors
///
/// Returns `Error::SessionVariableMappingNotFound` if any policy contains
/// a session variable pattern without a corresponding SQLite function mapping.
pub fn validate_table_policies<O: TranslationOptions, DB: DatabaseLike>(
    table: &DB::Table,
    schema: &DB,
    options: &O,
) -> Result<(), Error>
where
    DB::Table: TableLike<DB = DB>,
    DB::Policy: PolicyLike<DB = DB>,
{
    for policy in table.policies(schema) {
        if let Some(using_expr) = policy.using_expression(schema) {
            validate_session_variables(using_expr, options, table.table_name(), policy.name())?;
        }
        if let Some(check_expr) = policy.check_expression(schema) {
            validate_session_variables(check_expr, options, table.table_name(), policy.name())?;
        }
    }
    Ok(())
}

/// Transforms an expression AST by:
/// 1. Replacing session variable patterns with their SQLite function
///    equivalents
/// 2. Optionally prefixing column references with NEW. or OLD.
/// 3. Renaming table references from `table_name` to `inner_table_name` (for
///    RLS views)
#[allow(clippy::too_many_lines)]
fn transform_expr<O: TranslationOptions, DB: DatabaseLike>(
    expr: &Expr,
    options: &O,
    table: &DB::Table,
    schema: &DB,
    prefix: Option<&str>,
    table_rename: Option<(&str, &str)>,
) -> Expr
where
    DB::Table: TableLike<DB = DB>,
{
    match expr {
        // Handle current_setting('name')::type -> sqlite_func()
        Expr::Cast { expr: inner, .. } => {
            if let Expr::Function(func) = inner.as_ref()
                && let Some(transformed) = try_transform_session_function(func, options)
            {
                return transformed;
            }
            // Recursively transform the inner expression, removing the cast
            transform_expr(inner, options, table, schema, prefix, table_rename)
        }

        // Handle current_setting('name') without cast, and current_user as a function
        Expr::Function(func) => {
            if let Some(transformed) = try_transform_session_function(func, options) {
                return transformed;
            }

            // Check if it's current_user (parsed as a no-arg function in PostgreSQL)
            let func_name = func.name.to_string().to_lowercase();
            if func_name == "current_user"
                && let Some(sqlite_func) =
                    options.find_session_variable_function(&SessionVariablePattern::CurrentUser)
            {
                return make_function_call(sqlite_func);
            }

            // Not a session function, return as-is (could recurse into args if needed)
            Expr::Function(func.clone())
        }

        // Handle bare column identifiers -> PREFIX.column
        Expr::Identifier(ident) => {
            let ident_lower = ident.value.to_lowercase();

            // Check if it's current_user
            if ident_lower == "current_user"
                && let Some(sqlite_func) =
                    options.find_session_variable_function(&SessionVariablePattern::CurrentUser)
            {
                return make_function_call(sqlite_func);
            }

            // Check if it's a column that needs prefixing
            if let Some(pfx) = prefix
                && table.columns(schema).any(|c| c.column_name().to_lowercase() == ident_lower)
            {
                return Expr::CompoundIdentifier(vec![Ident::new(pfx), ident.clone()]);
            }

            Expr::Identifier(ident.clone())
        }

        // Handle already-qualified identifiers (e.g., table.column) - may need table rename or
        // prefix
        Expr::CompoundIdentifier(idents) => {
            if let Some((old_name, new_name)) = table_rename
                && idents.len() >= 2
                && idents[0].value.to_lowercase() == old_name.to_lowercase()
            {
                // This is a reference to the target table (e.g., ownable_owners.owner_id)
                // In trigger context with a prefix (NEW/OLD), use NEW.column or OLD.column
                if let Some(pfx) = prefix {
                    let mut new_idents = idents.clone();
                    new_idents[0] = Ident::new(pfx);
                    return Expr::CompoundIdentifier(new_idents);
                }
                // Otherwise rename to the _rls table
                let mut new_idents = idents.clone();
                new_idents[0] = Ident::new(new_name);
                return Expr::CompoundIdentifier(new_idents);
            }
            Expr::CompoundIdentifier(idents.clone())
        }

        // Recursively handle binary operations
        Expr::BinaryOp { left, op, right } => {
            Expr::BinaryOp {
                left: Box::new(transform_expr(left, options, table, schema, prefix, table_rename)),
                op: op.clone(),
                right: Box::new(transform_expr(
                    right,
                    options,
                    table,
                    schema,
                    prefix,
                    table_rename,
                )),
            }
        }

        // Recursively handle unary operations
        Expr::UnaryOp { op, expr: inner } => {
            Expr::UnaryOp {
                op: *op,
                expr: Box::new(transform_expr(inner, options, table, schema, prefix, table_rename)),
            }
        }

        // Handle nested/parenthesized expressions
        Expr::Nested(inner) => {
            Expr::Nested(Box::new(transform_expr(
                inner,
                options,
                table,
                schema,
                prefix,
                table_rename,
            )))
        }

        // Handle IS NULL / IS NOT NULL
        Expr::IsNull(inner) => {
            Expr::IsNull(Box::new(transform_expr(
                inner,
                options,
                table,
                schema,
                prefix,
                table_rename,
            )))
        }
        Expr::IsNotNull(inner) => {
            Expr::IsNotNull(Box::new(transform_expr(
                inner,
                options,
                table,
                schema,
                prefix,
                table_rename,
            )))
        }

        // Handle IN lists
        Expr::InList { expr: inner, list, negated } => {
            Expr::InList {
                expr: Box::new(transform_expr(inner, options, table, schema, prefix, table_rename)),
                list: list
                    .iter()
                    .map(|e| transform_expr(e, options, table, schema, prefix, table_rename))
                    .collect(),
                negated: *negated,
            }
        }

        // Handle BETWEEN
        Expr::Between { expr: inner, negated, low, high } => {
            Expr::Between {
                expr: Box::new(transform_expr(inner, options, table, schema, prefix, table_rename)),
                negated: *negated,
                low: Box::new(transform_expr(low, options, table, schema, prefix, table_rename)),
                high: Box::new(transform_expr(high, options, table, schema, prefix, table_rename)),
            }
        }

        // Handle EXISTS (subquery) - recursively transform the subquery's selection
        Expr::Exists { subquery, negated } => {
            Expr::Exists {
                subquery: Box::new(transform_query(
                    subquery,
                    options,
                    table,
                    schema,
                    prefix,
                    table_rename,
                )),
                negated: *negated,
            }
        }

        // Handle subquery expressions
        Expr::Subquery(subquery) => {
            Expr::Subquery(Box::new(transform_query(
                subquery,
                options,
                table,
                schema,
                prefix,
                table_rename,
            )))
        }

        // Handle IN (subquery) expressions
        Expr::InSubquery { expr: inner, subquery, negated } => {
            Expr::InSubquery {
                expr: Box::new(transform_expr(inner, options, table, schema, prefix, table_rename)),
                subquery: Box::new(transform_query(
                    subquery,
                    options,
                    table,
                    schema,
                    prefix,
                    table_rename,
                )),
                negated: *negated,
            }
        }

        // For any other expression type, return as-is
        other => other.clone(),
    }
}

/// Transforms an expression for UPDATE WITH CHECK clauses.
/// Uses COALESCE(NEW.column, OLD.column) for column references to handle
/// partial updates, since in SQLite INSTEAD OF triggers, NEW.column is only
/// defined for columns in the SET clause.
#[allow(clippy::too_many_lines)]
fn transform_expr_for_update_check<O: TranslationOptions, DB: DatabaseLike>(
    expr: &Expr,
    options: &O,
    table: &DB::Table,
    schema: &DB,
    table_rename: Option<(&str, &str)>,
) -> Expr
where
    DB::Table: TableLike<DB = DB>,
{
    match expr {
        // Handle current_setting('name')::type -> sqlite_func()
        Expr::Cast { expr: inner, .. } => {
            if let Expr::Function(func) = inner.as_ref()
                && let Some(transformed) = try_transform_session_function(func, options)
            {
                return transformed;
            }
            transform_expr_for_update_check(inner, options, table, schema, table_rename)
        }

        // Handle current_setting('name') without cast, and current_user as a function
        Expr::Function(func) => {
            if let Some(transformed) = try_transform_session_function(func, options) {
                return transformed;
            }

            let func_name = func.name.to_string().to_lowercase();
            if func_name == "current_user"
                && let Some(sqlite_func) =
                    options.find_session_variable_function(&SessionVariablePattern::CurrentUser)
            {
                return make_function_call(sqlite_func);
            }

            Expr::Function(func.clone())
        }

        // Handle bare column identifiers -> COALESCE(NEW.column, OLD.column)
        Expr::Identifier(ident) => {
            let ident_lower = ident.value.to_lowercase();

            // Check if it's current_user
            if ident_lower == "current_user"
                && let Some(sqlite_func) =
                    options.find_session_variable_function(&SessionVariablePattern::CurrentUser)
            {
                return make_function_call(sqlite_func);
            }

            // Check if it's a column that needs COALESCE wrapping
            if table.columns(schema).any(|c| c.column_name().to_lowercase() == ident_lower) {
                return make_coalesce_expr(ident);
            }

            Expr::Identifier(ident.clone())
        }

        // Handle already-qualified identifiers (e.g., table.column)
        Expr::CompoundIdentifier(idents) => {
            if let Some((old_name, _new_name)) = table_rename
                && idents.len() >= 2
                && idents[0].value.to_lowercase() == old_name.to_lowercase()
            {
                // Reference to the target table - use COALESCE
                return make_coalesce_expr(&idents[1]);
            }
            Expr::CompoundIdentifier(idents.clone())
        }

        // Recursively handle binary operations
        Expr::BinaryOp { left, op, right } => {
            Expr::BinaryOp {
                left: Box::new(transform_expr_for_update_check(
                    left,
                    options,
                    table,
                    schema,
                    table_rename,
                )),
                op: op.clone(),
                right: Box::new(transform_expr_for_update_check(
                    right,
                    options,
                    table,
                    schema,
                    table_rename,
                )),
            }
        }

        // Recursively handle unary operations
        Expr::UnaryOp { op, expr: inner } => {
            Expr::UnaryOp {
                op: *op,
                expr: Box::new(transform_expr_for_update_check(
                    inner,
                    options,
                    table,
                    schema,
                    table_rename,
                )),
            }
        }

        // Handle nested/parenthesized expressions
        Expr::Nested(inner) => {
            Expr::Nested(Box::new(transform_expr_for_update_check(
                inner,
                options,
                table,
                schema,
                table_rename,
            )))
        }

        // Handle IS NULL / IS NOT NULL
        Expr::IsNull(inner) => {
            Expr::IsNull(Box::new(transform_expr_for_update_check(
                inner,
                options,
                table,
                schema,
                table_rename,
            )))
        }
        Expr::IsNotNull(inner) => {
            Expr::IsNotNull(Box::new(transform_expr_for_update_check(
                inner,
                options,
                table,
                schema,
                table_rename,
            )))
        }

        // Handle IN lists
        Expr::InList { expr: inner, list, negated } => {
            Expr::InList {
                expr: Box::new(transform_expr_for_update_check(
                    inner,
                    options,
                    table,
                    schema,
                    table_rename,
                )),
                list: list
                    .iter()
                    .map(|e| {
                        transform_expr_for_update_check(e, options, table, schema, table_rename)
                    })
                    .collect(),
                negated: *negated,
            }
        }

        // Handle EXISTS (subquery) - delegate to transform_query for proper handling
        Expr::Exists { subquery, negated } => {
            Expr::Exists {
                subquery: Box::new(transform_query(
                    subquery,
                    options,
                    table,
                    schema,
                    Some("NEW"), // Use NEW prefix for subquery column refs
                    table_rename,
                )),
                negated: *negated,
            }
        }

        // Handle subquery expressions
        Expr::Subquery(subquery) => {
            Expr::Subquery(Box::new(transform_query(
                subquery,
                options,
                table,
                schema,
                Some("NEW"),
                table_rename,
            )))
        }

        // Handle IN (subquery) expressions
        Expr::InSubquery { expr: inner, subquery, negated } => {
            Expr::InSubquery {
                expr: Box::new(transform_expr_for_update_check(
                    inner,
                    options,
                    table,
                    schema,
                    table_rename,
                )),
                subquery: Box::new(transform_query(
                    subquery,
                    options,
                    table,
                    schema,
                    Some("NEW"),
                    table_rename,
                )),
                negated: *negated,
            }
        }

        // Handle BETWEEN
        Expr::Between { expr: inner, negated, low, high } => {
            Expr::Between {
                expr: Box::new(transform_expr_for_update_check(
                    inner,
                    options,
                    table,
                    schema,
                    table_rename,
                )),
                negated: *negated,
                low: Box::new(transform_expr_for_update_check(
                    low,
                    options,
                    table,
                    schema,
                    table_rename,
                )),
                high: Box::new(transform_expr_for_update_check(
                    high,
                    options,
                    table,
                    schema,
                    table_rename,
                )),
            }
        }

        // For any other expression type, return as-is
        other => other.clone(),
    }
}

/// Creates a COALESCE(NEW.column, OLD.column) expression.
fn make_coalesce_expr(column: &Ident) -> Expr {
    use sqlparser::ast::{FunctionArg, FunctionArgExpr, FunctionArgumentList, ObjectNamePart};

    let new_ref = Expr::CompoundIdentifier(vec![Ident::new("NEW"), column.clone()]);
    let old_ref = Expr::CompoundIdentifier(vec![Ident::new("OLD"), column.clone()]);

    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("COALESCE"))]),
        args: FunctionArguments::List(FunctionArgumentList {
            args: vec![
                FunctionArg::Unnamed(FunctionArgExpr::Expr(new_ref)),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(old_ref)),
            ],
            duplicate_treatment: None,
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
        uses_odbc_syntax: false,
    })
}

/// Transforms a Query (used in subqueries) by recursively transforming its
/// WHERE clause and FROM clause table references.
///
/// Inside a trigger context:
/// - References to the outer table (e.g., `ownables.id`) should become `OLD.id`
///   or `NEW.id` (using the prefix)
/// - Other tables in the FROM clause get renamed to `_rls` suffix if they have
///   RLS
fn transform_query<O: TranslationOptions, DB: DatabaseLike>(
    query: &sqlparser::ast::Query,
    options: &O,
    table: &DB::Table,
    schema: &DB,
    prefix: Option<&str>,
    outer_table: Option<(&str, &str)>,
) -> sqlparser::ast::Query
where
    DB::Table: TableLike<DB = DB>,
{
    let mut transformed = query.clone();
    let rls_suffix = options.get_rls_table_suffix();

    // Transform the body of the query (SELECT, etc.)
    if let sqlparser::ast::SetExpr::Select(ref mut select) = *transformed.body {
        // Collect table renames from the FROM clause
        let mut subquery_table_renames: Vec<(String, String)> = Vec::new();

        // Rename tables in the FROM clause to use _rls suffix
        // (In PostgreSQL, RLS policies query raw tables, not RLS-filtered views)
        for from in &mut select.from {
            if let sqlparser::ast::TableFactor::Table { name, .. } = &mut from.relation {
                let old_name = name.to_string();
                // Skip if already has RLS suffix to prevent double-suffix
                if old_name.ends_with(rls_suffix) {
                    subquery_table_renames.push((old_name.clone(), old_name));
                    continue;
                }
                // Only rename tables that have RLS policies (look up in schema)
                let has_rls =
                    schema.table(None, &old_name).is_some_and(|t| t.has_row_level_security(schema));
                if !has_rls {
                    subquery_table_renames.push((old_name.clone(), old_name));
                    continue;
                }
                let new_name = format!("{old_name}{rls_suffix}");
                subquery_table_renames.push((old_name, new_name.clone()));

                if let Ok(mut stmts) = sqlparser::parser::Parser::parse_sql(
                    &sqlparser::dialect::SQLiteDialect {},
                    &format!("SELECT * FROM {new_name}"),
                ) && let Some(sqlparser::ast::Statement::Query(q)) = stmts.pop()
                    && let sqlparser::ast::SetExpr::Select(s) = *q.body
                    && let Some(f) = s.from.first()
                    && let sqlparser::ast::TableFactor::Table { name: new_obj_name, .. } =
                        &f.relation
                {
                    *name = new_obj_name.clone();
                }
            }
        }

        // Transform the WHERE clause
        if let Some(ref selection) = select.selection {
            let mut transformed_selection = selection.clone();

            // For references to the outer table (e.g., `ownables.id`):
            // - In trigger context (prefix is Some): convert to OLD.id or NEW.id
            // - In view context (prefix is None): rename to ownables_rls.id
            if let Some((outer_table_name, renamed_table_name)) = outer_table {
                transformed_selection = transform_outer_table_refs(
                    &transformed_selection,
                    outer_table_name,
                    prefix,
                    Some(renamed_table_name),
                );
            }

            // Apply subquery table renames (tables in the FROM clause of this subquery)
            // We also pass the prefix here to handle unqualified column references
            // (e.g., `ownable_id` in subquery should become `NEW.ownable_id`)
            for (old, new) in &subquery_table_renames {
                transformed_selection = transform_expr(
                    &transformed_selection,
                    options,
                    table,
                    schema,
                    prefix, // Pass prefix to handle bare column identifiers
                    Some((old.as_str(), new.as_str())),
                );
            }

            // Transform session variables in the subquery
            // Also pass prefix for any remaining bare column identifiers
            transformed_selection =
                transform_expr(&transformed_selection, options, table, schema, prefix, None);

            select.selection = Some(transformed_selection);
        }
    }

    transformed
}

/// Transforms references to the outer table to use the prefix (OLD/NEW) or
/// rename.
///
/// - If prefix is Some("OLD") or Some("NEW"): `ownables.id` -> `OLD.id` or
///   `NEW.id`
/// - If prefix is None: `ownables.id` -> `ownables_rls.id` (using
///   renamed_table)
#[allow(clippy::too_many_lines)]
fn transform_outer_table_refs(
    expr: &Expr,
    outer_table_name: &str,
    prefix: Option<&str>,
    renamed_table: Option<&str>,
) -> Expr {
    match expr {
        Expr::CompoundIdentifier(idents) => {
            // Check if this is a reference to the outer table
            if idents.len() >= 2
                && idents[0].value.to_lowercase() == outer_table_name.to_lowercase()
            {
                let mut new_idents = idents.clone();
                if let Some(pfx) = prefix {
                    // In trigger context: ownables.id -> OLD.id or NEW.id
                    new_idents[0] = Ident::new(pfx);
                } else if let Some(renamed) = renamed_table {
                    // In view context: ownables.id -> ownables_rls.id
                    new_idents[0] = Ident::new(renamed);
                }
                return Expr::CompoundIdentifier(new_idents);
            }
            Expr::CompoundIdentifier(idents.clone())
        }

        Expr::BinaryOp { left, op, right } => {
            Expr::BinaryOp {
                left: Box::new(transform_outer_table_refs(
                    left,
                    outer_table_name,
                    prefix,
                    renamed_table,
                )),
                op: op.clone(),
                right: Box::new(transform_outer_table_refs(
                    right,
                    outer_table_name,
                    prefix,
                    renamed_table,
                )),
            }
        }

        Expr::UnaryOp { op, expr: inner } => {
            Expr::UnaryOp {
                op: *op,
                expr: Box::new(transform_outer_table_refs(
                    inner,
                    outer_table_name,
                    prefix,
                    renamed_table,
                )),
            }
        }

        Expr::Nested(inner) => {
            Expr::Nested(Box::new(transform_outer_table_refs(
                inner,
                outer_table_name,
                prefix,
                renamed_table,
            )))
        }

        Expr::IsNull(inner) => {
            Expr::IsNull(Box::new(transform_outer_table_refs(
                inner,
                outer_table_name,
                prefix,
                renamed_table,
            )))
        }

        Expr::IsNotNull(inner) => {
            Expr::IsNotNull(Box::new(transform_outer_table_refs(
                inner,
                outer_table_name,
                prefix,
                renamed_table,
            )))
        }

        Expr::InList { expr: inner, list, negated } => {
            Expr::InList {
                expr: Box::new(transform_outer_table_refs(
                    inner,
                    outer_table_name,
                    prefix,
                    renamed_table,
                )),
                list: list
                    .iter()
                    .map(|e| transform_outer_table_refs(e, outer_table_name, prefix, renamed_table))
                    .collect(),
                negated: *negated,
            }
        }

        Expr::Between { expr: inner, negated, low, high } => {
            Expr::Between {
                expr: Box::new(transform_outer_table_refs(
                    inner,
                    outer_table_name,
                    prefix,
                    renamed_table,
                )),
                negated: *negated,
                low: Box::new(transform_outer_table_refs(
                    low,
                    outer_table_name,
                    prefix,
                    renamed_table,
                )),
                high: Box::new(transform_outer_table_refs(
                    high,
                    outer_table_name,
                    prefix,
                    renamed_table,
                )),
            }
        }

        other => other.clone(),
    }
}

/// Tries to transform a function call if it's a session variable pattern.
/// Returns Some(transformed_expr) if it was a session function, None otherwise.
fn try_transform_session_function<O: TranslationOptions>(
    func: &Function,
    options: &O,
) -> Option<Expr> {
    let func_name = func.name.to_string().to_lowercase();

    if func_name == "current_setting"
        && let FunctionArguments::List(FunctionArgumentList { args, .. }) = &func.args
        && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(value)))) = args.first()
    {
        let value_str = value.to_string();
        let setting_name = value_str.trim_matches('\'');
        let pattern = SessionVariablePattern::CurrentSetting { name: setting_name.to_string() };

        if let Some(sqlite_func) = options.find_session_variable_function(&pattern) {
            return Some(make_function_call(sqlite_func));
        }
    }

    None
}

/// Creates a simple function call expression with no arguments: func_name()
fn make_function_call(func_name: &str) -> Expr {
    use sqlparser::ast::ObjectNamePart;
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(func_name))]),
        args: FunctionArguments::List(FunctionArgumentList {
            args: vec![],
            duplicate_treatment: None,
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
        uses_odbc_syntax: false,
    })
}

/// Generates the CREATE VIEW SQL statement for a table with RLS.
///
/// # Errors
///
/// This function is infallible but returns a `Result` for API consistency
/// with other RLS generation functions.
#[allow(clippy::unnecessary_wraps)]
pub fn generate_rls_view_sql<O: TranslationOptions, DB: DatabaseLike>(
    table: &DB::Table,
    schema: &DB,
    options: &O,
) -> Result<String, Error>
where
    DB::Table: TableLike<DB = DB>,
    DB::Policy: PolicyLike<DB = DB>,
{
    let ctx = RlsTriggerContext::new::<O, DB>(table, options);
    let table_name = ctx.table_name;
    let inner_table_name = &ctx.inner_table_name;
    let table_rename = Some(ctx.as_rename_tuple());

    // Collect SELECT policies for this table
    let select_policies = filter_policies(table, schema, &[CreatePolicyCommand::Select]);

    // Get all column names from the table for the SELECT clause
    let columns = collect_column_names(table, schema);
    let column_list = columns.join(", ");

    // Build the WHERE clause by combining all USING expressions
    let where_clause = if select_policies.is_empty() {
        String::new()
    } else {
        let mut conditions = Vec::new();
        for policy in &select_policies {
            if let Some(using_expr) = policy.using_expression(schema) {
                // Transform the AST, renaming table refs from table_name to inner_table_name
                let transformed =
                    transform_expr(using_expr, options, table, schema, None, table_rename);
                conditions.push(format!("({transformed})"));
            }
        }
        if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" OR "))
        }
    };

    Ok(format!(
        "CREATE VIEW {table_name} AS SELECT {column_list} FROM {inner_table_name}{where_clause}"
    ))
}

/// Generates INSTEAD OF INSERT trigger SQL.
fn generate_insert_trigger_sql<O: TranslationOptions, DB: DatabaseLike>(
    table: &DB::Table,
    schema: &DB,
    options: &O,
) -> String
where
    DB::Table: TableLike<DB = DB>,
    DB::Policy: PolicyLike<DB = DB>,
{
    let ctx = RlsTriggerContext::new::<O, DB>(table, options);
    let table_name = ctx.table_name;
    let inner_table_name = &ctx.inner_table_name;
    let table_rename = Some(ctx.as_rename_tuple());

    // Find INSERT policies
    let insert_policies = filter_policies(table, schema, &[CreatePolicyCommand::Insert]);

    // Get all column names for the INSERT statement
    let columns = collect_column_names(table, schema);
    let column_list = columns.join(", ");
    let value_list = columns.iter().map(|c| format!("NEW.{c}")).collect::<Vec<_>>().join(", ");

    // Build WITH CHECK expression - transform AST with NEW. prefix
    let check_conditions: Vec<String> = insert_policies
        .iter()
        .filter_map(|policy| {
            policy.check_expression(schema).map(|expr| {
                let transformed =
                    transform_expr(expr, options, table, schema, Some("NEW"), table_rename);
                format!("({transformed})")
            })
        })
        .collect();

    let trigger_body = if check_conditions.is_empty() {
        format!(
            "BEGIN\n    INSERT INTO {inner_table_name} ({column_list}) VALUES ({value_list});\nEND"
        )
    } else {
        let check = check_conditions.join(" OR ");
        format!(
            "BEGIN\n    SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}') WHERE NOT ({check});\n    INSERT INTO {inner_table_name} ({column_list}) VALUES ({value_list});\nEND"
        )
    };

    format!(
        "CREATE TRIGGER {table_name}_insert_trigger INSTEAD OF INSERT ON {table_name} FOR EACH ROW {trigger_body}"
    )
}

/// Generates INSTEAD OF UPDATE trigger SQL.
fn generate_update_trigger_sql<O: TranslationOptions, DB: DatabaseLike>(
    table: &DB::Table,
    schema: &DB,
    options: &O,
) -> String
where
    DB::Table: TableLike<DB = DB>,
    DB::Policy: PolicyLike<DB = DB>,
{
    let ctx = RlsTriggerContext::new::<O, DB>(table, options);
    let table_name = ctx.table_name;
    let inner_table_name = &ctx.inner_table_name;
    let table_rename = Some(ctx.as_rename_tuple());

    // Find UPDATE policies
    let update_policies = filter_policies(table, schema, &[CreatePolicyCommand::Update]);

    // Get all column names for the SET clause
    let columns = collect_column_names(table, schema);

    // Get primary key columns
    let pk_columns = collect_pk_column_names(table, schema);

    // Build SET clause - use COALESCE to handle partial updates
    // In SQLite INSTEAD OF triggers, NEW.column is only defined for columns in the
    // SET clause
    let set_clause = columns
        .iter()
        .map(|c| format!("{c} = COALESCE(NEW.{c}, OLD.{c})"))
        .collect::<Vec<_>>()
        .join(", ");

    // Build PK WHERE clause
    let pk_where = build_row_identity_clause(&columns, &pk_columns);

    // Build USING expression (filter which rows can be updated) - use OLD. prefix
    let using_conditions: Vec<String> = update_policies
        .iter()
        .filter_map(|policy| {
            policy.using_expression(schema).map(|expr| {
                let transformed =
                    transform_expr(expr, options, table, schema, Some("OLD"), table_rename);
                format!("({transformed})")
            })
        })
        .collect();

    // Build WITH CHECK expression - use COALESCE(NEW.col, OLD.col) for partial
    // updates In SQLite INSTEAD OF triggers, NEW.column is only defined for
    // columns in SET clause
    let check_conditions: Vec<String> = update_policies
        .iter()
        .filter_map(|policy| {
            policy.check_expression(schema).map(|expr| {
                let transformed =
                    transform_expr_for_update_check(expr, options, table, schema, table_rename);
                format!("({transformed})")
            })
        })
        .collect();

    // Combine WHERE clause
    let full_where = if using_conditions.is_empty() {
        pk_where
    } else {
        let using = using_conditions.join(" OR ");
        format!("({pk_where}) AND ({using})")
    };

    let trigger_body = if check_conditions.is_empty() {
        format!("BEGIN\n    UPDATE {inner_table_name} SET {set_clause} WHERE {full_where};\nEND")
    } else {
        let check = check_conditions.join(" OR ");
        format!(
            "BEGIN\n    SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}') WHERE NOT ({check});\n    UPDATE {inner_table_name} SET {set_clause} WHERE {full_where};\nEND"
        )
    };

    format!(
        "CREATE TRIGGER {table_name}_update_trigger INSTEAD OF UPDATE ON {table_name} FOR EACH ROW {trigger_body}"
    )
}

/// Generates INSTEAD OF DELETE trigger SQL.
fn generate_delete_trigger_sql<O: TranslationOptions, DB: DatabaseLike>(
    table: &DB::Table,
    schema: &DB,
    options: &O,
) -> String
where
    DB::Table: TableLike<DB = DB>,
    DB::Policy: PolicyLike<DB = DB>,
{
    let ctx = RlsTriggerContext::new::<O, DB>(table, options);
    let table_name = ctx.table_name;
    let inner_table_name = &ctx.inner_table_name;
    let table_rename = Some(ctx.as_rename_tuple());

    // Find DELETE policies
    let delete_policies = filter_policies(table, schema, &[CreatePolicyCommand::Delete]);

    // Get all column names for the WHERE clause fallback
    let columns = collect_column_names(table, schema);

    // Get primary key columns
    let pk_columns = collect_pk_column_names(table, schema);

    // Build PK WHERE clause
    let pk_where = build_row_identity_clause(&columns, &pk_columns);

    // Build USING expression - use OLD. prefix for delete
    let using_conditions: Vec<String> = delete_policies
        .iter()
        .filter_map(|policy| {
            policy.using_expression(schema).map(|expr| {
                let transformed =
                    transform_expr(expr, options, table, schema, Some("OLD"), table_rename);
                format!("({transformed})")
            })
        })
        .collect();

    // Combine WHERE clause
    let full_where = if using_conditions.is_empty() {
        pk_where
    } else {
        let using = using_conditions.join(" OR ");
        format!("({pk_where}) AND ({using})")
    };

    let trigger_body =
        format!("BEGIN\n    DELETE FROM {inner_table_name} WHERE {full_where};\nEND");

    format!(
        "CREATE TRIGGER {table_name}_delete_trigger INSTEAD OF DELETE ON {table_name} FOR EACH ROW {trigger_body}"
    )
}

/// Generates all RLS-related SQL statements for a table.
///
/// # Errors
///
/// Returns an error if the generated SQL cannot be parsed by the SQLite dialect
/// parser.
pub fn generate_rls_statements<O: TranslationOptions, DB: DatabaseLike>(
    table: &DB::Table,
    schema: &DB,
    options: &O,
) -> Result<Vec<Statement>, Error>
where
    DB::Table: TableLike<DB = DB>,
    DB::Policy: PolicyLike<DB = DB>,
{
    let dialect = sqlparser::dialect::SQLiteDialect {};
    let mut statements = Vec::new();

    // Generate view
    let view_sql = generate_rls_view_sql(table, schema, options)?;
    let view_stmts = sqlparser::parser::Parser::parse_sql(&dialect, &view_sql)
        .map_err(|e| Error::UnknownPostgresFeature(format!("Failed to parse view SQL: {e}")))?;
    statements.extend(view_stmts);

    // Generate INSERT trigger
    let insert_sql = generate_insert_trigger_sql(table, schema, options);
    let insert_stmts =
        sqlparser::parser::Parser::parse_sql(&dialect, &insert_sql).map_err(|e| {
            Error::UnknownPostgresFeature(format!("Failed to parse insert trigger: {e}"))
        })?;
    statements.extend(insert_stmts);

    // Generate UPDATE trigger
    let update_sql = generate_update_trigger_sql(table, schema, options);
    let update_stmts =
        sqlparser::parser::Parser::parse_sql(&dialect, &update_sql).map_err(|e| {
            Error::UnknownPostgresFeature(format!("Failed to parse update trigger: {e}"))
        })?;
    statements.extend(update_stmts);

    // Generate DELETE trigger
    let delete_sql = generate_delete_trigger_sql(table, schema, options);
    let delete_stmts =
        sqlparser::parser::Parser::parse_sql(&dialect, &delete_sql).map_err(|e| {
            Error::UnknownPostgresFeature(format!("Failed to parse delete trigger: {e}"))
        })?;
    statements.extend(delete_stmts);

    Ok(statements)
}

/// Generates SQLite statements for a read-only RLS view (no write triggers).
///
/// This is used for tables where the session user role only has SELECT
/// permission. The backing table is created for sync purposes, but no INSTEAD
/// OF triggers are generated since the user cannot write to this table.
///
/// # Errors
///
/// Returns an error if the generated SQL cannot be parsed by the SQLite dialect
/// parser.
pub fn generate_readonly_rls_statements<O: TranslationOptions, DB: DatabaseLike>(
    table: &DB::Table,
    schema: &DB,
    options: &O,
) -> Result<Vec<Statement>, Error>
where
    DB::Table: TableLike<DB = DB>,
    DB::Policy: PolicyLike<DB = DB>,
{
    let dialect = sqlparser::dialect::SQLiteDialect {};
    let mut statements = Vec::new();

    // Generate view only (no write triggers)
    let view_sql = generate_rls_view_sql(table, schema, options)?;
    let view_stmts = sqlparser::parser::Parser::parse_sql(&dialect, &view_sql)
        .map_err(|e| Error::UnknownPostgresFeature(format!("Failed to parse view SQL: {e}")))?;
    statements.extend(view_stmts);

    Ok(statements)
}

/// Renames a CREATE TABLE statement to use the inner table name for RLS.
/// Also updates any foreign key references to other RLS tables.
#[must_use]
pub fn rename_table_for_rls<O: TranslationOptions, DB: DatabaseLike>(
    create_table: &CreateTable,
    options: &O,
    _schema: &DB,
) -> CreateTable
where
    DB::Table: TableLike<DB = DB>,
{
    let suffix = options.get_rls_table_suffix();
    let mut renamed = create_table.clone();
    // Get the current table name and append suffix
    let current_name = renamed.name.to_string();
    let new_name = format!("{current_name}{suffix}");

    // Parse the new name back into an ObjectName
    let dialect = sqlparser::dialect::PostgreSqlDialect {};
    if let Ok(mut stmts) =
        sqlparser::parser::Parser::parse_sql(&dialect, &format!("SELECT * FROM {new_name}"))
        && let Some(Statement::Query(query)) = stmts.pop()
        && let sqlparser::ast::SetExpr::Select(select) = *query.body
        && let Some(from) = select.from.first()
        && let sqlparser::ast::TableFactor::Table { name, .. } = &from.relation
    {
        renamed.name = name.clone();
    }

    renamed
}
