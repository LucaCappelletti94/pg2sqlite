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

/// Transforms an expression AST by:
/// 1. Replacing session variable patterns with their SQLite function
///    equivalents
/// 2. Optionally prefixing column references with NEW. or OLD.
fn transform_expr<O: TranslationOptions>(
    expr: &Expr,
    options: &O,
    columns: &[String],
    prefix: Option<&str>,
) -> Expr {
    match expr {
        // Handle current_setting('name')::type -> sqlite_func()
        Expr::Cast { expr: inner, .. } => {
            if let Expr::Function(func) = inner.as_ref()
                && let Some(transformed) = try_transform_session_function(func, options)
            {
                return transformed;
            }
            // Recursively transform the inner expression, removing the cast
            transform_expr(inner, options, columns, prefix)
        }

        // Handle current_setting('name') without cast
        Expr::Function(func) => {
            if let Some(transformed) = try_transform_session_function(func, options) {
                return transformed;
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
                && columns.iter().any(|c| c.to_lowercase() == ident_lower)
            {
                return Expr::CompoundIdentifier(vec![Ident::new(pfx), ident.clone()]);
            }

            Expr::Identifier(ident.clone())
        }

        // Handle already-qualified identifiers (e.g., table.column)
        Expr::CompoundIdentifier(idents) => Expr::CompoundIdentifier(idents.clone()),

        // Recursively handle binary operations
        Expr::BinaryOp { left, op, right } => {
            Expr::BinaryOp {
                left: Box::new(transform_expr(left, options, columns, prefix)),
                op: op.clone(),
                right: Box::new(transform_expr(right, options, columns, prefix)),
            }
        }

        // Recursively handle unary operations
        Expr::UnaryOp { op, expr: inner } => {
            Expr::UnaryOp {
                op: *op,
                expr: Box::new(transform_expr(inner, options, columns, prefix)),
            }
        }

        // Handle nested/parenthesized expressions
        Expr::Nested(inner) => {
            Expr::Nested(Box::new(transform_expr(inner, options, columns, prefix)))
        }

        // Handle IS NULL / IS NOT NULL
        Expr::IsNull(inner) => {
            Expr::IsNull(Box::new(transform_expr(inner, options, columns, prefix)))
        }
        Expr::IsNotNull(inner) => {
            Expr::IsNotNull(Box::new(transform_expr(inner, options, columns, prefix)))
        }

        // Handle IN lists
        Expr::InList { expr: inner, list, negated } => {
            Expr::InList {
                expr: Box::new(transform_expr(inner, options, columns, prefix)),
                list: list.iter().map(|e| transform_expr(e, options, columns, prefix)).collect(),
                negated: *negated,
            }
        }

        // Handle BETWEEN
        Expr::Between { expr: inner, negated, low, high } => {
            Expr::Between {
                expr: Box::new(transform_expr(inner, options, columns, prefix)),
                negated: *negated,
                low: Box::new(transform_expr(low, options, columns, prefix)),
                high: Box::new(transform_expr(high, options, columns, prefix)),
            }
        }

        // For any other expression type, return as-is
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
    let table_name = table.table_name();
    let inner_table_name = format!("{}{}", table_name, options.get_rls_table_suffix());

    // Collect SELECT policies for this table
    let select_policies: Vec<_> = table
        .policies(schema)
        .filter(|p| matches!(p.command(), CreatePolicyCommand::Select | CreatePolicyCommand::All))
        .collect();

    // Get all column names from the table
    let columns: Vec<_> = table.columns(schema).map(|c| c.column_name().to_string()).collect();

    let column_list = columns.join(", ");

    // Build the WHERE clause by combining all USING expressions
    let where_clause = if select_policies.is_empty() {
        String::new()
    } else {
        let mut conditions = Vec::new();
        for policy in &select_policies {
            if let Some(using_expr) = policy.using_expression(schema) {
                // Transform the AST directly, no column prefix needed for view WHERE
                let transformed = transform_expr(using_expr, options, &columns, None);
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
    let table_name = table.table_name();
    let inner_table_name = format!("{}{}", table_name, options.get_rls_table_suffix());

    // Find INSERT policies
    let insert_policies: Vec<_> = table
        .policies(schema)
        .filter(|p| matches!(p.command(), CreatePolicyCommand::Insert | CreatePolicyCommand::All))
        .collect();

    // Get all column names
    let columns: Vec<_> = table.columns(schema).map(|c| c.column_name().to_string()).collect();

    let column_list = columns.join(", ");
    let value_list = columns.iter().map(|c| format!("NEW.{c}")).collect::<Vec<_>>().join(", ");

    // Build WITH CHECK expression - transform AST with NEW. prefix
    let check_conditions: Vec<String> = insert_policies
        .iter()
        .filter_map(|policy| {
            policy.check_expression(schema).map(|expr| {
                let transformed = transform_expr(expr, options, &columns, Some("NEW"));
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
            "BEGIN\n    SELECT RAISE(ABORT, 'new row violates row-level security policy') WHERE NOT ({check});\n    INSERT INTO {inner_table_name} ({column_list}) VALUES ({value_list});\nEND"
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
    let table_name = table.table_name();
    let inner_table_name = format!("{}{}", table_name, options.get_rls_table_suffix());

    // Find UPDATE policies
    let update_policies: Vec<_> = table
        .policies(schema)
        .filter(|p| matches!(p.command(), CreatePolicyCommand::Update | CreatePolicyCommand::All))
        .collect();

    // Get all column names
    let columns: Vec<_> = table.columns(schema).map(|c| c.column_name().to_string()).collect();

    // Get primary key columns
    let pk_columns: Vec<_> =
        table.primary_key_columns(schema).map(|c| c.column_name().to_string()).collect();

    // Build SET clause
    let set_clause =
        columns.iter().map(|c| format!("{c} = NEW.{c}")).collect::<Vec<_>>().join(", ");

    // Build PK WHERE clause
    let pk_where = if pk_columns.is_empty() {
        columns.iter().map(|c| format!("{c} = OLD.{c}")).collect::<Vec<_>>().join(" AND ")
    } else {
        pk_columns.iter().map(|c| format!("{c} = OLD.{c}")).collect::<Vec<_>>().join(" AND ")
    };

    // Build USING expression (filter which rows can be updated) - use OLD. prefix
    let using_conditions: Vec<String> = update_policies
        .iter()
        .filter_map(|policy| {
            policy.using_expression(schema).map(|expr| {
                let transformed = transform_expr(expr, options, &columns, Some("OLD"));
                format!("({transformed})")
            })
        })
        .collect();

    // Build WITH CHECK expression - use NEW. prefix
    let check_conditions: Vec<String> = update_policies
        .iter()
        .filter_map(|policy| {
            policy.check_expression(schema).map(|expr| {
                let transformed = transform_expr(expr, options, &columns, Some("NEW"));
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
            "BEGIN\n    SELECT RAISE(ABORT, 'new row violates row-level security policy') WHERE NOT ({check});\n    UPDATE {inner_table_name} SET {set_clause} WHERE {full_where};\nEND"
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
    let table_name = table.table_name();
    let inner_table_name = format!("{}{}", table_name, options.get_rls_table_suffix());

    // Find DELETE policies
    let delete_policies: Vec<_> = table
        .policies(schema)
        .filter(|p| matches!(p.command(), CreatePolicyCommand::Delete | CreatePolicyCommand::All))
        .collect();

    // Get all column names
    let columns: Vec<_> = table.columns(schema).map(|c| c.column_name().to_string()).collect();

    // Get primary key columns
    let pk_columns: Vec<_> =
        table.primary_key_columns(schema).map(|c| c.column_name().to_string()).collect();

    // Build PK WHERE clause
    let pk_where = if pk_columns.is_empty() {
        columns.iter().map(|c| format!("{c} = OLD.{c}")).collect::<Vec<_>>().join(" AND ")
    } else {
        pk_columns.iter().map(|c| format!("{c} = OLD.{c}")).collect::<Vec<_>>().join(" AND ")
    };

    // Build USING expression - use OLD. prefix for delete
    let using_conditions: Vec<String> = delete_policies
        .iter()
        .filter_map(|policy| {
            policy.using_expression(schema).map(|expr| {
                let transformed = transform_expr(expr, options, &columns, Some("OLD"));
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

/// Renames a CREATE TABLE statement to use the inner table name for RLS.
#[must_use]
pub fn rename_table_for_rls(create_table: &CreateTable, suffix: &str) -> CreateTable {
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
