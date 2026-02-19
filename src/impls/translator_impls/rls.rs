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

    const fn as_rename_tuple(&self) -> (&str, &str) {
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

/// Strategy for handling column references in expression transformation.
///
/// - `Prefix`: Prefixes bare column references with `NEW.`/`OLD.` and renames
///   qualified table refs. Used in most trigger contexts.
/// - `Coalesce`: Wraps column references in `COALESCE(NEW.col, OLD.col)`. Used
///   in UPDATE WITH CHECK clauses where NEW.column is only defined for columns
///   in the SET clause.
enum ColumnRefStrategy<'a> {
    Prefix { prefix: Option<&'a str>, table_rename: Option<(&'a str, &'a str)> },
    Coalesce { table_rename: Option<(&'a str, &'a str)> },
}

impl ColumnRefStrategy<'_> {
    fn table_rename(&self) -> Option<(&str, &str)> {
        match self {
            ColumnRefStrategy::Prefix { table_rename, .. }
            | ColumnRefStrategy::Coalesce { table_rename, .. } => *table_rename,
        }
    }

    /// The prefix to use when transforming subqueries. Coalesce strategy
    /// always uses `Some("NEW")` for subqueries.
    fn subquery_prefix(&self) -> Option<&str> {
        match self {
            ColumnRefStrategy::Prefix { prefix, .. } => *prefix,
            ColumnRefStrategy::Coalesce { .. } => Some("NEW"),
        }
    }
}

/// Core expression transformation, generic over column reference strategy.
///
/// 1. Replaces session variable patterns with their SQLite function equivalents
/// 2. Handles column references according to the given `ColumnRefStrategy`
/// 3. Recursively transforms all sub-expressions
#[allow(clippy::too_many_lines)]
fn transform_expr_generic<O: TranslationOptions, DB: DatabaseLike>(
    expr: &Expr,
    options: &O,
    table: &DB::Table,
    schema: &DB,
    strategy: &ColumnRefStrategy<'_>,
) -> Expr
where
    DB::Table: TableLike<DB = DB>,
{
    let recurse =
        |e: &Expr| -> Expr { transform_expr_generic(e, options, table, schema, strategy) };

    match expr {
        // Handle current_setting('name')::type -> sqlite_func()
        Expr::Cast { expr: inner, .. } => {
            if let Expr::Function(func) = inner.as_ref()
                && let Some(transformed) = try_transform_session_function(func, options)
            {
                return transformed;
            }
            recurse(inner)
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

        // Handle bare column identifiers
        Expr::Identifier(ident) => {
            let ident_lower = ident.value.to_lowercase();

            if ident_lower == "current_user"
                && let Some(sqlite_func) =
                    options.find_session_variable_function(&SessionVariablePattern::CurrentUser)
            {
                return make_function_call(sqlite_func);
            }

            match strategy {
                ColumnRefStrategy::Prefix { prefix: Some(pfx), .. } => {
                    if table.columns(schema).any(|c| c.column_name().to_lowercase() == ident_lower)
                    {
                        return Expr::CompoundIdentifier(vec![Ident::new(*pfx), ident.clone()]);
                    }
                }
                ColumnRefStrategy::Coalesce { .. } => {
                    if table.columns(schema).any(|c| c.column_name().to_lowercase() == ident_lower)
                    {
                        return make_coalesce_expr(ident);
                    }
                }
                ColumnRefStrategy::Prefix { prefix: None, .. } => {}
            }

            Expr::Identifier(ident.clone())
        }

        // Handle already-qualified identifiers (e.g., table.column)
        Expr::CompoundIdentifier(idents) => {
            if let Some((old_name, new_name)) = strategy.table_rename()
                && idents.len() >= 2
                && idents[0].value.to_lowercase() == old_name.to_lowercase()
            {
                match strategy {
                    ColumnRefStrategy::Prefix { prefix: Some(pfx), .. } => {
                        let mut new_idents = idents.clone();
                        new_idents[0] = Ident::new(*pfx);
                        return Expr::CompoundIdentifier(new_idents);
                    }
                    ColumnRefStrategy::Prefix { prefix: None, .. } => {
                        let mut new_idents = idents.clone();
                        new_idents[0] = Ident::new(new_name);
                        return Expr::CompoundIdentifier(new_idents);
                    }
                    ColumnRefStrategy::Coalesce { .. } => {
                        return make_coalesce_expr(&idents[1]);
                    }
                }
            }
            Expr::CompoundIdentifier(idents.clone())
        }

        // Recursively handle binary operations
        Expr::BinaryOp { left, op, right } => {
            Expr::BinaryOp {
                left: Box::new(recurse(left)),
                op: op.clone(),
                right: Box::new(recurse(right)),
            }
        }

        Expr::UnaryOp { op, expr: inner } => {
            Expr::UnaryOp { op: *op, expr: Box::new(recurse(inner)) }
        }

        Expr::Nested(inner) => Expr::Nested(Box::new(recurse(inner))),

        Expr::IsNull(inner) => Expr::IsNull(Box::new(recurse(inner))),
        Expr::IsNotNull(inner) => Expr::IsNotNull(Box::new(recurse(inner))),

        Expr::InList { expr: inner, list, negated } => {
            Expr::InList {
                expr: Box::new(recurse(inner)),
                list: list.iter().map(recurse).collect(),
                negated: *negated,
            }
        }

        Expr::Between { expr: inner, negated, low, high } => {
            Expr::Between {
                expr: Box::new(recurse(inner)),
                negated: *negated,
                low: Box::new(recurse(low)),
                high: Box::new(recurse(high)),
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
                    strategy.subquery_prefix(),
                    strategy.table_rename(),
                )),
                negated: *negated,
            }
        }

        Expr::Subquery(subquery) => {
            Expr::Subquery(Box::new(transform_query(
                subquery,
                options,
                table,
                schema,
                strategy.subquery_prefix(),
                strategy.table_rename(),
            )))
        }

        Expr::InSubquery { expr: inner, subquery, negated } => {
            Expr::InSubquery {
                expr: Box::new(recurse(inner)),
                subquery: Box::new(transform_query(
                    subquery,
                    options,
                    table,
                    schema,
                    strategy.subquery_prefix(),
                    strategy.table_rename(),
                )),
                negated: *negated,
            }
        }

        // For any other expression type, return as-is
        other => other.clone(),
    }
}

/// Transforms an expression AST by:
/// 1. Replacing session variable patterns with their SQLite function
///    equivalents
/// 2. Optionally prefixing column references with NEW. or OLD.
/// 3. Renaming table references from `table_name` to `inner_table_name` (for
///    RLS views)
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
    transform_expr_generic(
        expr,
        options,
        table,
        schema,
        &ColumnRefStrategy::Prefix { prefix, table_rename },
    )
}

/// Transforms an expression for UPDATE WITH CHECK clauses.
/// Uses COALESCE(NEW.column, OLD.column) for column references to handle
/// partial updates, since in SQLite INSTEAD OF triggers, NEW.column is only
/// defined for columns in the SET clause.
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
    transform_expr_generic(
        expr,
        options,
        table,
        schema,
        &ColumnRefStrategy::Coalesce { table_rename },
    )
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
    // Validate that audit table name is configured
    let audit_table_name =
        options.get_rls_audit_table_name().ok_or(Error::RlsAuditTableNameRequired)?;

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

    // Generate RLS validation monitoring triggers and views
    let validation_stmts =
        generate_rls_validation_statements(table, schema, options, audit_table_name)?;
    statements.extend(validation_stmts);

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
    // Validate that audit table name is configured
    let audit_table_name =
        options.get_rls_audit_table_name().ok_or(Error::RlsAuditTableNameRequired)?;

    let dialect = sqlparser::dialect::SQLiteDialect {};
    let mut statements = Vec::new();

    // Generate view only (no write triggers)
    let view_sql = generate_rls_view_sql(table, schema, options)?;
    let view_stmts = sqlparser::parser::Parser::parse_sql(&dialect, &view_sql)
        .map_err(|e| Error::UnknownPostgresFeature(format!("Failed to parse view SQL: {e}")))?;
    statements.extend(view_stmts);

    // Generate RLS validation monitoring triggers and views
    // (even for read-only tables, we monitor sync operations)
    let validation_stmts =
        generate_rls_validation_statements(table, schema, options, audit_table_name)?;
    statements.extend(validation_stmts);

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

// ============================================================================
// RLS Validation and Monitoring
// ============================================================================

/// Error message prefix used in RLS validation triggers.
const RLS_VALIDATION_ERROR: &str = "RLS validation";

/// Generates the SQL to create the RLS audit table.
///
/// This table stores all detected RLS policy violations during sync operations.
/// The audit table is created once and shared by all RLS-enabled tables.
///
/// # Schema
/// - `id`: Auto-incrementing primary key
/// - `table_name`: Name of the table where violation occurred
/// - `violation_type`: Type of violation (always 'rls_policy_violation')
/// - `row_identifier`: Primary key values of the violating row
/// - `policy_name`: Name of the policy that was violated
/// - `detected_at`: Timestamp when violation was detected
/// - `severity`: Severity level ('warning' in monitor mode, 'error' in strict)
/// - `details`: Additional contextual information
/// - `reported_at`: Timestamp when violation was reported to backend (NULL
///   until reported)
///
/// # Arguments
/// * `audit_table_name` - The name to use for the audit table (user-configured)
///
/// # Returns
/// SQL string to create the audit table with STRICT mode for better type
/// safety.
#[must_use]
pub fn generate_audit_table_sql(audit_table_name: &str) -> String {
    format!(
        r"CREATE TABLE IF NOT EXISTS {audit_table_name} (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    table_name TEXT NOT NULL,
    violation_type TEXT NOT NULL,
    row_identifier TEXT NOT NULL,
    policy_name TEXT,
    detected_at TEXT NOT NULL,
    severity TEXT NOT NULL,
    details TEXT,
    reported_at TEXT
) STRICT"
    )
}

/// Builds an expression that identifies a row using its primary key columns.
///
/// For example, if a table has primary key columns `(id, tenant_id)`, this
/// generates: `'id=' || quote(NEW.id) || ', tenant_id=' ||
/// quote(NEW.tenant_id)`
///
/// # Arguments
/// * `pk_columns` - List of primary key column names
/// * `prefix` - Row reference prefix ("NEW" or "OLD")
///
/// # Returns
/// SQLite expression that builds a human-readable identifier string.
fn build_row_identifier_expr(pk_columns: &[String], prefix: &str) -> String {
    if pk_columns.is_empty() {
        return "'<no PK>'".to_string();
    }

    pk_columns
        .iter()
        .map(|col| format!("'{col}=' || quote({prefix}.{col})"))
        .collect::<Vec<_>>()
        .join(" || ', ' || ")
}

/// Generates a WHERE clause check that tests if a row is visible through the
/// RLS view.
///
/// This builds an EXISTS subquery that checks if the inserted/updated row would
/// be visible when querying through the RLS-filtered view.
///
/// # Arguments
/// * `table_name` - Name of the RLS view (e.g., "documents")
/// * `pk_columns` - Primary key columns for row identification
/// * `prefix` - Row reference prefix ("NEW" or "OLD")
///
/// # Returns
/// SQL expression: `EXISTS (SELECT 1 FROM view WHERE pk_match)`
fn generate_row_visibility_check(table_name: &str, pk_columns: &[String], prefix: &str) -> String {
    let where_clause = if pk_columns.is_empty() {
        // No PK - check all rows (will be slow but correct)
        "1=1".to_string()
    } else {
        pk_columns
            .iter()
            .map(|col| format!("{table_name}.{col} = {prefix}.{col}"))
            .collect::<Vec<_>>()
            .join(" AND ")
    };

    format!("EXISTS (SELECT 1 FROM {table_name} WHERE {where_clause})")
}

/// Generates an AFTER INSERT or AFTER UPDATE trigger that monitors for RLS
/// violations.
///
/// This trigger fires after a row is inserted into or updated in the backing
/// table (e.g., `documents_rls`). It checks if the row is visible through the
/// RLS view (e.g., `documents`). If not visible, the row violates RLS policy.
///
/// In monitor mode: logs violation to audit table
/// In strict mode: logs violation + aborts transaction
///
/// # Arguments
/// * `table_name` - Name of the RLS view (e.g., "documents")
/// * `inner_table_name` - Name of the backing table (e.g., "documents_rls")
/// * `pk_columns` - Primary key column names
/// * `audit_table_name` - Name of the audit table for logging
/// * `strict_mode` - If true, add RAISE(ABORT) to block violations
/// * `operation` - Either "insert" or "update"
///
/// # Returns
/// SQL string for CREATE TRIGGER statement
fn generate_monitoring_trigger_sql(
    table_name: &str,
    inner_table_name: &str,
    pk_columns: &[String],
    audit_table_name: &str,
    strict_mode: bool,
    operation: &str,
) -> String {
    let visibility_check = generate_row_visibility_check(table_name, pk_columns, "NEW");
    let row_identifier = build_row_identifier_expr(pk_columns, "NEW");
    let severity = if strict_mode { "error" } else { "warning" };
    let op_upper = operation.to_uppercase();
    let past_participle = if operation == "insert" { "inserted into" } else { "updated in" };

    let abort_clause = if strict_mode {
        format!(
            r"
        SELECT RAISE(ABORT, '{RLS_VALIDATION_ERROR}: row violates row-level security policy for table ''{table_name}''');"
        )
    } else {
        String::new()
    };

    format!(
        r"CREATE TRIGGER {inner_table_name}_rls_monitor_{operation}
AFTER {op_upper} ON {inner_table_name}
FOR EACH ROW
BEGIN
    -- Check if {operation}d row is visible through RLS view
    INSERT INTO {audit_table_name} (
        table_name,
        violation_type,
        row_identifier,
        policy_name,
        detected_at,
        severity,
        details,
        reported_at
    )
    SELECT
        '{table_name}',
        'rls_policy_violation',
        {row_identifier},
        '{op_upper} policy',
        datetime('now'),
        '{severity}',
        'Row {past_participle} backing table but not visible through RLS view',
        NULL
    WHERE NOT ({visibility_check});{abort_clause}
END"
    )
}

/// Generates a view that shows all rows violating RLS policies.
///
/// This validation view makes it easy to query for violations without going
/// through the audit table. It shows all rows in the backing table that are NOT
/// visible through the RLS view.
///
/// # Arguments
/// * `table_name` - Name of the RLS view (e.g., "documents")
/// * `inner_table_name` - Name of the backing table (e.g., "documents_rls")
/// * `columns` - All column names in the table
/// * `pk_columns` - Primary key columns for matching rows
///
/// # Returns
/// SQL string for CREATE VIEW statement
fn generate_validation_view_sql(
    table_name: &str,
    inner_table_name: &str,
    columns: &[String],
    pk_columns: &[String],
) -> String {
    let column_list = columns.join(", ");

    // Build the WHERE clause to match rows by primary key
    let pk_match = if pk_columns.is_empty() {
        // No PK - this is rare but we'll use all columns (inefficient but correct)
        columns
            .iter()
            .map(|col| format!("{inner_table_name}.{col} = {table_name}.{col}"))
            .collect::<Vec<_>>()
            .join(" AND ")
    } else {
        pk_columns
            .iter()
            .map(|col| format!("{inner_table_name}.{col} = {table_name}.{col}"))
            .collect::<Vec<_>>()
            .join(" AND ")
    };

    format!(
        r"CREATE VIEW {inner_table_name}_violations AS
SELECT {column_list}
FROM {inner_table_name}
WHERE NOT EXISTS (
    SELECT 1
    FROM {table_name}
    WHERE {pk_match}
)"
    )
}

/// Generates the complete set of RLS validation statements for a table.
///
/// This includes:
/// - AFTER INSERT monitoring trigger
/// - AFTER UPDATE monitoring trigger
/// - Validation view showing current violations
///
/// Note: The audit table itself is generated separately (once per schema).
///
/// # Arguments
/// * `table` - The table with RLS policies
/// * `schema` - The database schema
/// * `options` - Translation options (contains audit table name and strict mode
///   setting)
///
/// # Returns
/// Vector of SQL statements parsed and ready for execution
///
/// # Errors
/// Returns an error if the generated SQL cannot be parsed
pub fn generate_rls_validation_statements<O: TranslationOptions, DB: DatabaseLike>(
    table: &DB::Table,
    schema: &DB,
    options: &O,
    audit_table_name: &str,
) -> Result<Vec<Statement>, Error>
where
    DB::Table: TableLike<DB = DB>,
{
    let dialect = sqlparser::dialect::SQLiteDialect {};
    let mut statements = Vec::new();

    let table_name = table.table_name();
    let inner_table_name = format!("{}{}", table_name, options.get_rls_table_suffix());
    let pk_columns = collect_pk_column_names(table, schema);
    let all_columns = collect_column_names(table, schema);
    let strict_mode = options.is_strict_rls_validation();

    // Generate INSERT and UPDATE monitoring triggers
    for operation in &["insert", "update"] {
        let monitor_sql = generate_monitoring_trigger_sql(
            table_name,
            &inner_table_name,
            &pk_columns,
            audit_table_name,
            strict_mode,
            operation,
        );
        let stmts = sqlparser::parser::Parser::parse_sql(&dialect, &monitor_sql).map_err(|e| {
            Error::UnknownPostgresFeature(format!(
                "Failed to parse RLS {operation} monitor trigger: {e}"
            ))
        })?;
        statements.extend(stmts);
    }

    // Generate validation view
    let validation_view_sql =
        generate_validation_view_sql(table_name, &inner_table_name, &all_columns, &pk_columns);
    let view_stmts =
        sqlparser::parser::Parser::parse_sql(&dialect, &validation_view_sql).map_err(|e| {
            Error::UnknownPostgresFeature(format!("Failed to parse RLS validation view: {e}"))
        })?;
    statements.extend(view_stmts);

    Ok(statements)
}

/// Helper function to generate the audit table SQL as a Statement.
///
/// This is a convenience wrapper around `generate_audit_table_sql` that parses
/// the result into a Statement for easier integration.
///
/// # Arguments
/// * `audit_table_name` - The name to use for the audit table
///
/// # Returns
/// Parsed CREATE TABLE statement
///
/// # Errors
/// Returns an error if the generated SQL cannot be parsed
pub fn generate_rls_audit_table(audit_table_name: &str) -> Result<Statement, Error> {
    let dialect = sqlparser::dialect::SQLiteDialect {};
    let sql = generate_audit_table_sql(audit_table_name);

    let mut stmts = sqlparser::parser::Parser::parse_sql(&dialect, &sql).map_err(|e| {
        Error::UnknownPostgresFeature(format!("Failed to parse RLS audit table SQL: {e}"))
    })?;

    stmts.pop().ok_or_else(|| {
        Error::UnknownPostgresFeature("No statement generated for audit table".to_string())
    })
}
