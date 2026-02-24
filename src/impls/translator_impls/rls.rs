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
    FunctionArgumentList, FunctionArguments, Ident, JoinConstraint, JoinOperator, ObjectName,
    Statement, TableFactor, Value,
};

use crate::{
    errors::Error,
    impls::{
        generated_sql::{parse_generated_sql, parse_single_generated_sql},
        object_name::{
            append_suffix, last_ident, prefixed_quoted_identifier, quote_identifier,
            schema_and_table_for_lookup,
        },
    },
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
    identity_cols
        .iter()
        .map(|c| {
            let col = quote_identifier(c);
            let old_col = prefixed_quoted_identifier("OLD", c);
            format!("{col} = {old_col}")
        })
        .collect::<Vec<_>>()
        .join(" AND ")
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
    let mut policies = Vec::new();
    for policy in table.policies(schema) {
        let command = policy.command();
        if commands.contains(&command) || command == CreatePolicyCommand::All {
            policies.push(policy);
        }
    }
    policies
}

/// Context for RLS trigger generation, encapsulating common setup.
struct RlsTriggerContext<'a> {
    table_name: &'a str,
    inner_table_name: String,
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
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

fn push_pattern_unique(
    patterns: &mut Vec<SessionVariablePattern>,
    pattern: SessionVariablePattern,
) {
    if !patterns.contains(&pattern) {
        patterns.push(pattern);
    }
}

fn collect_patterns_from_function(func: &Function, patterns: &mut Vec<SessionVariablePattern>) {
    let func_name = function_name_lower(func);
    if func_name == "current_user" {
        push_pattern_unique(patterns, SessionVariablePattern::CurrentUser);
    }
    if let Some(setting_name) = extract_current_setting_name(func) {
        push_pattern_unique(
            patterns,
            SessionVariablePattern::CurrentSetting { name: setting_name },
        );
    }

    if let FunctionArguments::List(arg_list) = &func.args {
        for arg in &arg_list.args {
            match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))
                | FunctionArg::Named { arg: FunctionArgExpr::Expr(expr), .. }
                | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(expr), .. } => {
                    collect_session_variable_patterns(expr, patterns);
                }
                _ => {}
            }
        }
    }

    if let Some(filter) = &func.filter {
        collect_session_variable_patterns(filter, patterns);
    }

    if let Some(over) = &func.over
        && let sqlparser::ast::WindowType::WindowSpec(window_spec) = over
    {
        for expr in &window_spec.partition_by {
            collect_session_variable_patterns(expr, patterns);
        }
        for order_by_expr in &window_spec.order_by {
            collect_session_variable_patterns(&order_by_expr.expr, patterns);
        }
    }

    for order_by_expr in &func.within_group {
        collect_session_variable_patterns(&order_by_expr.expr, patterns);
    }
}

fn collect_patterns_from_table_factor(
    factor: &TableFactor,
    patterns: &mut Vec<SessionVariablePattern>,
) {
    match factor {
        TableFactor::Derived { subquery, .. } => {
            collect_patterns_from_query(subquery, patterns);
        }
        TableFactor::NestedJoin { table_with_joins, .. } => {
            collect_patterns_from_table_factor(&table_with_joins.relation, patterns);
            for join in &table_with_joins.joins {
                collect_patterns_from_table_factor(&join.relation, patterns);
            }
        }
        _ => {}
    }
}

fn collect_patterns_from_select(
    select: &sqlparser::ast::Select,
    patterns: &mut Vec<SessionVariablePattern>,
) {
    for table_with_joins in &select.from {
        collect_patterns_from_table_factor(&table_with_joins.relation, patterns);
        for join in &table_with_joins.joins {
            collect_patterns_from_table_factor(&join.relation, patterns);
            match &join.join_operator {
                JoinOperator::Join(JoinConstraint::On(expr))
                | JoinOperator::Inner(JoinConstraint::On(expr))
                | JoinOperator::Left(JoinConstraint::On(expr))
                | JoinOperator::LeftOuter(JoinConstraint::On(expr))
                | JoinOperator::Right(JoinConstraint::On(expr))
                | JoinOperator::RightOuter(JoinConstraint::On(expr))
                | JoinOperator::FullOuter(JoinConstraint::On(expr))
                | JoinOperator::CrossJoin(JoinConstraint::On(expr))
                | JoinOperator::Semi(JoinConstraint::On(expr))
                | JoinOperator::LeftSemi(JoinConstraint::On(expr))
                | JoinOperator::RightSemi(JoinConstraint::On(expr))
                | JoinOperator::Anti(JoinConstraint::On(expr))
                | JoinOperator::LeftAnti(JoinConstraint::On(expr))
                | JoinOperator::RightAnti(JoinConstraint::On(expr))
                | JoinOperator::StraightJoin(JoinConstraint::On(expr)) => {
                    collect_session_variable_patterns(expr, patterns);
                }
                JoinOperator::AsOf { constraint: JoinConstraint::On(expr), match_condition } => {
                    collect_session_variable_patterns(expr, patterns);
                    collect_session_variable_patterns(match_condition, patterns);
                }
                JoinOperator::AsOf { match_condition, .. } => {
                    collect_session_variable_patterns(match_condition, patterns);
                }
                _ => {}
            }
        }
    }

    if let Some(selection) = &select.selection {
        collect_session_variable_patterns(selection, patterns);
    }
    if let Some(having) = &select.having {
        collect_session_variable_patterns(having, patterns);
    }
    if let Some(qualify) = &select.qualify {
        collect_session_variable_patterns(qualify, patterns);
    }

    for item in &select.projection {
        if let sqlparser::ast::SelectItem::UnnamedExpr(expr)
        | sqlparser::ast::SelectItem::ExprWithAlias { expr, .. } = item
        {
            collect_session_variable_patterns(expr, patterns);
        }
    }

    if let sqlparser::ast::GroupByExpr::Expressions(exprs, _) = &select.group_by {
        for expr in exprs {
            collect_session_variable_patterns(expr, patterns);
        }
    }
}

fn collect_patterns_from_set_expr(
    set_expr: &sqlparser::ast::SetExpr,
    patterns: &mut Vec<SessionVariablePattern>,
) {
    match set_expr {
        sqlparser::ast::SetExpr::Select(select) => collect_patterns_from_select(select, patterns),
        sqlparser::ast::SetExpr::Query(query) => collect_patterns_from_query(query, patterns),
        sqlparser::ast::SetExpr::SetOperation { left, right, .. } => {
            collect_patterns_from_set_expr(left, patterns);
            collect_patterns_from_set_expr(right, patterns);
        }
        sqlparser::ast::SetExpr::Values(values) => {
            for row in &values.rows {
                for expr in row {
                    collect_session_variable_patterns(expr, patterns);
                }
            }
        }
        _ => {}
    }
}

fn collect_patterns_from_query(
    query: &sqlparser::ast::Query,
    patterns: &mut Vec<SessionVariablePattern>,
) {
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            collect_patterns_from_query(&cte.query, patterns);
        }
    }
    collect_patterns_from_set_expr(query.body.as_ref(), patterns);

    if let Some(order_by) = &query.order_by
        && let sqlparser::ast::OrderByKind::Expressions(exprs) = &order_by.kind
    {
        for order_expr in exprs {
            collect_session_variable_patterns(&order_expr.expr, patterns);
        }
    }

    if let Some(limit_clause) = &query.limit_clause {
        match limit_clause {
            sqlparser::ast::LimitClause::LimitOffset { limit, offset, limit_by } => {
                if let Some(limit) = limit {
                    collect_session_variable_patterns(limit, patterns);
                }
                if let Some(offset) = offset {
                    collect_session_variable_patterns(&offset.value, patterns);
                }
                for expr in limit_by {
                    collect_session_variable_patterns(expr, patterns);
                }
            }
            sqlparser::ast::LimitClause::OffsetCommaLimit { offset, limit } => {
                collect_session_variable_patterns(offset, patterns);
                collect_session_variable_patterns(limit, patterns);
            }
        }
    }

    if let Some(fetch) = &query.fetch
        && let Some(quantity) = &fetch.quantity
    {
        collect_session_variable_patterns(quantity, patterns);
    }
}

fn collect_patterns_from_expr_pair(
    left: &Expr,
    right: &Expr,
    patterns: &mut Vec<SessionVariablePattern>,
) {
    collect_session_variable_patterns(left, patterns);
    collect_session_variable_patterns(right, patterns);
}

fn collect_patterns_from_expr_slice(exprs: &[Expr], patterns: &mut Vec<SessionVariablePattern>) {
    for expr in exprs {
        collect_session_variable_patterns(expr, patterns);
    }
}

fn collect_patterns_from_case_expr(
    operand: Option<&Expr>,
    conditions: &[sqlparser::ast::CaseWhen],
    else_result: Option<&Expr>,
    patterns: &mut Vec<SessionVariablePattern>,
) {
    if let Some(operand) = operand {
        collect_session_variable_patterns(operand, patterns);
    }
    for case_when in conditions {
        collect_session_variable_patterns(&case_when.condition, patterns);
        collect_session_variable_patterns(&case_when.result, patterns);
    }
    if let Some(else_result) = else_result {
        collect_session_variable_patterns(else_result, patterns);
    }
}

fn collect_patterns_from_trim_expr(
    expr: &Expr,
    trim_what: Option<&Expr>,
    trim_characters: Option<&[Expr]>,
    patterns: &mut Vec<SessionVariablePattern>,
) {
    collect_session_variable_patterns(expr, patterns);
    if let Some(trim_what) = trim_what {
        collect_session_variable_patterns(trim_what, patterns);
    }
    if let Some(trim_characters) = trim_characters {
        collect_patterns_from_expr_slice(trim_characters, patterns);
    }
}

fn collect_patterns_from_substring_expr(
    expr: &Expr,
    substring_from: Option<&Expr>,
    substring_for: Option<&Expr>,
    patterns: &mut Vec<SessionVariablePattern>,
) {
    collect_session_variable_patterns(expr, patterns);
    if let Some(substring_from) = substring_from {
        collect_session_variable_patterns(substring_from, patterns);
    }
    if let Some(substring_for) = substring_for {
        collect_session_variable_patterns(substring_for, patterns);
    }
}

fn collect_patterns_from_overlay_expr(
    expr: &Expr,
    overlay_what: &Expr,
    overlay_from: &Expr,
    overlay_for: Option<&Expr>,
    patterns: &mut Vec<SessionVariablePattern>,
) {
    collect_session_variable_patterns(expr, patterns);
    collect_session_variable_patterns(overlay_what, patterns);
    collect_session_variable_patterns(overlay_from, patterns);
    if let Some(overlay_for) = overlay_for {
        collect_session_variable_patterns(overlay_for, patterns);
    }
}

fn collect_patterns_from_compound_access(
    root: &Expr,
    access_chain: &[sqlparser::ast::AccessExpr],
    patterns: &mut Vec<SessionVariablePattern>,
) {
    collect_session_variable_patterns(root, patterns);
    for access in access_chain {
        if let sqlparser::ast::AccessExpr::Dot(expr) = access {
            collect_session_variable_patterns(expr, patterns);
        }
    }
}

/// Collect all session variable patterns used by an expression tree.
fn collect_session_variable_patterns(expr: &Expr, patterns: &mut Vec<SessionVariablePattern>) {
    match expr {
        Expr::Identifier(ident) => {
            if ident.value.eq_ignore_ascii_case("current_user") {
                push_pattern_unique(patterns, SessionVariablePattern::CurrentUser);
            }
        }
        Expr::Function(func) => collect_patterns_from_function(func, patterns),
        Expr::Subquery(query) => collect_patterns_from_query(query, patterns),
        Expr::Exists { subquery, .. } => collect_patterns_from_query(subquery, patterns),
        Expr::InSubquery { expr, subquery, .. } => {
            collect_session_variable_patterns(expr, patterns);
            collect_patterns_from_query(subquery, patterns);
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::AnyOp { left, right, .. }
        | Expr::AllOp { left, right, .. } => {
            collect_patterns_from_expr_pair(left, right, patterns);
        }
        Expr::Like { expr, pattern, .. }
        | Expr::ILike { expr, pattern, .. }
        | Expr::SimilarTo { expr, pattern, .. }
        | Expr::RLike { expr, pattern, .. }
        | Expr::Position { expr, r#in: pattern }
        | Expr::AtTimeZone { timestamp: expr, time_zone: pattern }
        | Expr::IsDistinctFrom(expr, pattern)
        | Expr::IsNotDistinctFrom(expr, pattern) => {
            collect_patterns_from_expr_pair(expr, pattern, patterns);
        }
        Expr::UnaryOp { expr, .. }
        | Expr::Cast { expr, .. }
        | Expr::Extract { expr, .. }
        | Expr::Ceil { expr, .. }
        | Expr::Floor { expr, .. }
        | Expr::Nested(expr)
        | Expr::IsNull(expr)
        | Expr::IsNotNull(expr)
        | Expr::IsUnknown(expr)
        | Expr::IsNotUnknown(expr)
        | Expr::IsTrue(expr)
        | Expr::IsNotTrue(expr)
        | Expr::IsFalse(expr)
        | Expr::IsNotFalse(expr)
        | Expr::Prefixed { value: expr, .. }
        | Expr::Collate { expr, .. }
        | Expr::IsNormalized { expr, .. } => collect_session_variable_patterns(expr, patterns),
        Expr::Tuple(exprs) | Expr::Array(sqlparser::ast::Array { elem: exprs, .. }) => {
            collect_patterns_from_expr_slice(exprs, patterns);
        }
        Expr::InList { expr, list, .. } => {
            collect_session_variable_patterns(expr, patterns);
            collect_patterns_from_expr_slice(list, patterns);
        }
        Expr::Between { expr, low, high, .. } => {
            collect_session_variable_patterns(expr, patterns);
            collect_patterns_from_expr_pair(low, high, patterns);
        }
        Expr::Case { operand, conditions, else_result, .. } => {
            collect_patterns_from_case_expr(
                operand.as_deref(),
                conditions,
                else_result.as_deref(),
                patterns,
            )
        }
        Expr::Trim { expr, trim_what, trim_characters, .. } => {
            collect_patterns_from_trim_expr(
                expr,
                trim_what.as_deref(),
                trim_characters.as_deref(),
                patterns,
            );
        }
        Expr::Substring { expr, substring_from, substring_for, .. } => {
            collect_patterns_from_substring_expr(
                expr,
                substring_from.as_deref(),
                substring_for.as_deref(),
                patterns,
            );
        }
        Expr::Overlay { expr, overlay_what, overlay_from, overlay_for } => {
            collect_patterns_from_overlay_expr(
                expr,
                overlay_what,
                overlay_from,
                overlay_for.as_deref(),
                patterns,
            );
        }
        Expr::CompoundFieldAccess { root, access_chain } => {
            collect_patterns_from_compound_access(root, access_chain, patterns);
        }
        Expr::Interval(interval) => collect_session_variable_patterns(&interval.value, patterns),
        _ => {}
    }
}

fn function_name_lower(func: &Function) -> String {
    func.name
        .0
        .last()
        .and_then(|part| part.as_ident())
        .map_or_else(String::new, |ident| ident.value.to_lowercase())
}

fn extract_string_literal(expr: &Expr) -> Option<String> {
    if let Expr::Value(value) = expr {
        match &value.value {
            Value::SingleQuotedString(s) | Value::DoubleQuotedString(s) => Some(s.clone()),
            _ => None,
        }
    } else {
        None
    }
}

fn extract_current_setting_name(func: &Function) -> Option<String> {
    if function_name_lower(func) != "current_setting" {
        return None;
    }
    if let FunctionArguments::List(FunctionArgumentList { args, .. }) = &func.args {
        return args.first().and_then(|arg| {
            match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))
                | FunctionArg::Named { arg: FunctionArgExpr::Expr(expr), .. } => {
                    extract_string_literal(expr)
                }
                _ => None,
            }
        });
    }
    None
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
    let mut patterns = Vec::new();
    collect_session_variable_patterns(expr, &mut patterns);

    for pattern in patterns {
        if options.find_session_variable_function(&pattern).is_none() {
            return Err(Error::SessionVariableMappingNotFound {
                pattern: match pattern {
                    SessionVariablePattern::CurrentUser => {
                        format!("current_user in table '{table_name}', policy '{policy_name}'")
                    }
                    SessionVariablePattern::CurrentSetting { name } => {
                        format!(
                            "current_setting('{name}') in table '{table_name}', policy '{policy_name}'"
                        )
                    }
                },
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
    match expr {
        // Handle current_setting('name')::type -> sqlite_func()
        Expr::Cast { expr: inner, .. } => {
            if let Expr::Function(func) = inner.as_ref()
                && let Some(transformed) = try_transform_session_function(func, options)
            {
                return transformed;
            }
            transform_expr_generic(inner, options, table, schema, strategy)
        }

        // Handle current_setting('name') without cast, and current_user as a function
        Expr::Function(func) => {
            if let Some(transformed) = try_transform_session_function(func, options) {
                return transformed;
            }

            let func_name = function_name_lower(func);
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
                left: Box::new(transform_expr_generic(left, options, table, schema, strategy)),
                op: op.clone(),
                right: Box::new(transform_expr_generic(right, options, table, schema, strategy)),
            }
        }

        Expr::UnaryOp { op, expr: inner } => {
            Expr::UnaryOp {
                op: *op,
                expr: Box::new(transform_expr_generic(inner, options, table, schema, strategy)),
            }
        }

        Expr::Nested(inner) => {
            Expr::Nested(Box::new(transform_expr_generic(inner, options, table, schema, strategy)))
        }

        Expr::IsNull(inner) => {
            Expr::IsNull(Box::new(transform_expr_generic(inner, options, table, schema, strategy)))
        }
        Expr::IsNotNull(inner) => {
            Expr::IsNotNull(Box::new(transform_expr_generic(
                inner, options, table, schema, strategy,
            )))
        }

        Expr::InList { expr: inner, list, negated } => {
            Expr::InList {
                expr: Box::new(transform_expr_generic(inner, options, table, schema, strategy)),
                list: list
                    .iter()
                    .map(|item| transform_expr_generic(item, options, table, schema, strategy))
                    .collect(),
                negated: *negated,
            }
        }

        Expr::Between { expr: inner, negated, low, high } => {
            Expr::Between {
                expr: Box::new(transform_expr_generic(inner, options, table, schema, strategy)),
                negated: *negated,
                low: Box::new(transform_expr_generic(low, options, table, schema, strategy)),
                high: Box::new(transform_expr_generic(high, options, table, schema, strategy)),
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
                expr: Box::new(transform_expr_generic(inner, options, table, schema, strategy)),
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

/// Transforms a Query (used in subqueries) by recursively transforming table
/// references and all relevant expressions.
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
    let context =
        SubqueryTransformContext { options, table, schema, prefix, outer_table, rls_suffix };

    if let sqlparser::ast::SetExpr::Select(ref mut select) = *transformed.body {
        let mut subquery_table_renames: Vec<(String, String)> = Vec::new();
        for table_with_joins in &mut select.from {
            transform_table_with_joins_for_subquery(
                table_with_joins,
                &context,
                &mut subquery_table_renames,
            );
        }

        let rewrite_expr = |expr: &Expr| {
            transform_subquery_expression(
                expr,
                options,
                table,
                schema,
                prefix,
                outer_table,
                &subquery_table_renames,
            )
        };

        if let Some(selection) = &mut select.selection {
            *selection = rewrite_expr(selection);
        }

        for item in &mut select.projection {
            if let sqlparser::ast::SelectItem::UnnamedExpr(expr)
            | sqlparser::ast::SelectItem::ExprWithAlias { expr, .. } = item
            {
                *expr = rewrite_expr(expr);
            }
        }

        if let Some(having) = &mut select.having {
            *having = rewrite_expr(having);
        }

        if let Some(qualify) = &mut select.qualify {
            *qualify = rewrite_expr(qualify);
        }

        if let sqlparser::ast::GroupByExpr::Expressions(group_exprs, _) = &mut select.group_by {
            for group_expr in group_exprs {
                *group_expr = rewrite_expr(group_expr);
            }
        }
    }

    transformed
}

fn transform_subquery_expression<O: TranslationOptions, DB: DatabaseLike>(
    expr: &Expr,
    options: &O,
    table: &DB::Table,
    schema: &DB,
    prefix: Option<&str>,
    outer_table: Option<(&str, &str)>,
    subquery_table_renames: &[(String, String)],
) -> Expr
where
    DB::Table: TableLike<DB = DB>,
{
    let mut transformed = expr.clone();

    if let Some((outer_table_name, renamed_table_name)) = outer_table {
        transformed = transform_outer_table_refs(
            &transformed,
            outer_table_name,
            prefix,
            Some(renamed_table_name),
        );
    }

    for (old_name, new_name) in subquery_table_renames {
        transformed = transform_expr(
            &transformed,
            options,
            table,
            schema,
            prefix,
            Some((old_name.as_str(), new_name.as_str())),
        );
    }

    transform_expr(&transformed, options, table, schema, prefix, None)
}

struct SubqueryTransformContext<'a, O: TranslationOptions, DB: DatabaseLike>
where
    DB::Table: TableLike<DB = DB>,
{
    options: &'a O,
    table: &'a DB::Table,
    schema: &'a DB,
    prefix: Option<&'a str>,
    outer_table: Option<(&'a str, &'a str)>,
    rls_suffix: &'a str,
}

fn transform_table_with_joins_for_subquery<O: TranslationOptions, DB: DatabaseLike>(
    table_with_joins: &mut sqlparser::ast::TableWithJoins,
    context: &SubqueryTransformContext<'_, O, DB>,
    subquery_table_renames: &mut Vec<(String, String)>,
) where
    DB::Table: TableLike<DB = DB>,
{
    transform_table_factor_for_subquery(
        &mut table_with_joins.relation,
        context,
        subquery_table_renames,
    );

    for join in &mut table_with_joins.joins {
        transform_table_factor_for_subquery(&mut join.relation, context, subquery_table_renames);
        transform_join_operator_for_subquery(
            &mut join.join_operator,
            context,
            subquery_table_renames,
        );
    }
}

fn transform_table_factor_for_subquery<O: TranslationOptions, DB: DatabaseLike>(
    factor: &mut TableFactor,
    context: &SubqueryTransformContext<'_, O, DB>,
    subquery_table_renames: &mut Vec<(String, String)>,
) where
    DB::Table: TableLike<DB = DB>,
{
    match factor {
        TableFactor::Table { name, .. } => {
            let old_name =
                last_ident(name).map_or_else(|| name.to_string(), |ident| ident.value.clone());
            if old_name.ends_with(context.rls_suffix) {
                subquery_table_renames.push((old_name.clone(), old_name));
                return;
            }

            let (table_schema, table_name) = schema_and_table_for_lookup(name);
            let has_rls = table_name
                .and_then(|table_name| context.schema.table(table_schema, table_name))
                .is_some_and(|table| table.has_row_level_security(context.schema));
            if has_rls {
                let renamed_name = append_suffix(name, context.rls_suffix);
                let new_name = last_ident(&renamed_name)
                    .map_or_else(|| old_name.clone(), |ident| ident.value.clone());
                subquery_table_renames.push((old_name, new_name));
                *name = renamed_name;
            } else {
                subquery_table_renames.push((old_name.clone(), old_name));
            }
        }
        TableFactor::Derived { subquery, .. } => {
            **subquery = transform_query(
                subquery,
                context.options,
                context.table,
                context.schema,
                context.prefix,
                context.outer_table,
            );
        }
        TableFactor::NestedJoin { table_with_joins, .. } => {
            transform_table_with_joins_for_subquery(
                table_with_joins,
                context,
                subquery_table_renames,
            );
        }
        _ => {}
    }
}

fn join_constraint_mut(join_operator: &mut JoinOperator) -> Option<&mut JoinConstraint> {
    match join_operator {
        JoinOperator::Join(constraint)
        | JoinOperator::Inner(constraint)
        | JoinOperator::Left(constraint)
        | JoinOperator::LeftOuter(constraint)
        | JoinOperator::Right(constraint)
        | JoinOperator::RightOuter(constraint)
        | JoinOperator::FullOuter(constraint)
        | JoinOperator::CrossJoin(constraint)
        | JoinOperator::Semi(constraint)
        | JoinOperator::LeftSemi(constraint)
        | JoinOperator::RightSemi(constraint)
        | JoinOperator::Anti(constraint)
        | JoinOperator::LeftAnti(constraint)
        | JoinOperator::RightAnti(constraint)
        | JoinOperator::StraightJoin(constraint)
        | JoinOperator::AsOf { constraint, .. } => Some(constraint),
        JoinOperator::CrossApply | JoinOperator::OuterApply => None,
    }
}

fn transform_join_operator_for_subquery<O: TranslationOptions, DB: DatabaseLike>(
    join_operator: &mut JoinOperator,
    context: &SubqueryTransformContext<'_, O, DB>,
    subquery_table_renames: &[(String, String)],
) where
    DB::Table: TableLike<DB = DB>,
{
    let rewrite_constraint = |constraint: &mut JoinConstraint| {
        if let JoinConstraint::On(expr) = constraint {
            *expr = transform_subquery_expression(
                expr,
                context.options,
                context.table,
                context.schema,
                context.prefix,
                context.outer_table,
                subquery_table_renames,
            );
        }
    };

    if let Some(constraint) = join_constraint_mut(join_operator) {
        rewrite_constraint(constraint);
    }

    if let JoinOperator::AsOf { match_condition, .. } = join_operator {
        *match_condition = transform_subquery_expression(
            match_condition,
            context.options,
            context.table,
            context.schema,
            context.prefix,
            context.outer_table,
            subquery_table_renames,
        );
    }
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
    if let Some(setting_name) = extract_current_setting_name(func) {
        let pattern = SessionVariablePattern::CurrentSetting { name: setting_name };
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
    let table_name_quoted = quote_identifier(table_name);
    let inner_table_name_quoted = quote_identifier(inner_table_name);

    // Collect SELECT policies for this table
    let select_policies = filter_policies(table, schema, &[CreatePolicyCommand::Select]);

    // Get all column names from the table for the SELECT clause
    let columns = collect_column_names(table, schema);
    let column_list =
        columns.iter().map(|column| quote_identifier(column)).collect::<Vec<_>>().join(", ");

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
        "CREATE VIEW {table_name_quoted} AS SELECT {column_list} FROM {inner_table_name_quoted}{where_clause}"
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
    let table_name_quoted = quote_identifier(table_name);
    let inner_table_name_quoted = quote_identifier(inner_table_name);
    let trigger_name = quote_identifier(&format!("{table_name}_insert_trigger"));

    // Find INSERT policies
    let insert_policies = filter_policies(table, schema, &[CreatePolicyCommand::Insert]);

    // Get all column names for the INSERT statement
    let columns = collect_column_names(table, schema);
    let column_list = columns.iter().map(|column| quote_identifier(column)).collect::<Vec<_>>().join(", ");
    let value_list = columns
        .iter()
        .map(|column| prefixed_quoted_identifier("NEW", column))
        .collect::<Vec<_>>()
        .join(", ");

    // Build WITH CHECK expression - transform AST with NEW. prefix
    let mut check_conditions = Vec::new();
    for policy in &insert_policies {
        if let Some(expr) = policy.check_expression(schema) {
            let transformed =
                transform_expr(expr, options, table, schema, Some("NEW"), table_rename);
            check_conditions.push(format!("({transformed})"));
        }
    }

    let trigger_body = if check_conditions.is_empty() {
        format!(
            "BEGIN\n    INSERT INTO {inner_table_name_quoted} ({column_list}) VALUES ({value_list});\nEND"
        )
    } else {
        let check = check_conditions.join(" OR ");
        format!(
            "BEGIN\n    SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}') WHERE NOT ({check});\n    INSERT INTO {inner_table_name_quoted} ({column_list}) VALUES ({value_list});\nEND"
        )
    };

    format!(
        "CREATE TRIGGER {trigger_name} INSTEAD OF INSERT ON {table_name_quoted} FOR EACH ROW {trigger_body}"
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
    let table_name_quoted = quote_identifier(table_name);
    let inner_table_name_quoted = quote_identifier(inner_table_name);
    let trigger_name = quote_identifier(&format!("{table_name}_update_trigger"));

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
        .map(|column| {
            let quoted_column = quote_identifier(column);
            let new_column = prefixed_quoted_identifier("NEW", column);
            let old_column = prefixed_quoted_identifier("OLD", column);
            format!("{quoted_column} = COALESCE({new_column}, {old_column})")
        })
        .collect::<Vec<_>>()
        .join(", ");

    // Build PK WHERE clause
    let pk_where = build_row_identity_clause(&columns, &pk_columns);

    // Build USING expression (filter which rows can be updated) - use OLD. prefix
    let mut using_conditions = Vec::new();
    for policy in &update_policies {
        if let Some(expr) = policy.using_expression(schema) {
            let transformed =
                transform_expr(expr, options, table, schema, Some("OLD"), table_rename);
            using_conditions.push(format!("({transformed})"));
        }
    }

    // Build WITH CHECK expression - use COALESCE(NEW.col, OLD.col) for partial
    // updates In SQLite INSTEAD OF triggers, NEW.column is only defined for
    // columns in SET clause
    let mut check_conditions = Vec::new();
    for policy in &update_policies {
        if let Some(expr) = policy.check_expression(schema) {
            let transformed =
                transform_expr_for_update_check(expr, options, table, schema, table_rename);
            check_conditions.push(format!("({transformed})"));
        }
    }

    // Combine WHERE clause
    let full_where = if using_conditions.is_empty() {
        pk_where
    } else {
        let using = using_conditions.join(" OR ");
        format!("({pk_where}) AND ({using})")
    };

    let trigger_body = if check_conditions.is_empty() {
        format!(
            "BEGIN\n    UPDATE {inner_table_name_quoted} SET {set_clause} WHERE {full_where};\nEND"
        )
    } else {
        let check = check_conditions.join(" OR ");
        format!(
            "BEGIN\n    SELECT RAISE(ABORT, '{RLS_VIOLATION_ERROR}') WHERE NOT ({check});\n    UPDATE {inner_table_name} SET {set_clause} WHERE {full_where};\nEND"
        )
    };

    format!(
        "CREATE TRIGGER {trigger_name} INSTEAD OF UPDATE ON {table_name_quoted} FOR EACH ROW {trigger_body}"
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
    let table_name_quoted = quote_identifier(table_name);
    let inner_table_name_quoted = quote_identifier(inner_table_name);
    let trigger_name = quote_identifier(&format!("{table_name}_delete_trigger"));

    // Find DELETE policies
    let delete_policies = filter_policies(table, schema, &[CreatePolicyCommand::Delete]);

    // Get all column names for the WHERE clause fallback
    let columns = collect_column_names(table, schema);

    // Get primary key columns
    let pk_columns = collect_pk_column_names(table, schema);

    // Build PK WHERE clause
    let pk_where = build_row_identity_clause(&columns, &pk_columns);

    // Build USING expression - use OLD. prefix for delete
    let mut using_conditions = Vec::new();
    for policy in &delete_policies {
        if let Some(expr) = policy.using_expression(schema) {
            let transformed =
                transform_expr(expr, options, table, schema, Some("OLD"), table_rename);
            using_conditions.push(format!("({transformed})"));
        }
    }

    // Combine WHERE clause
    let full_where = if using_conditions.is_empty() {
        pk_where
    } else {
        let using = using_conditions.join(" OR ");
        format!("({pk_where}) AND ({using})")
    };

    let trigger_body =
        format!("BEGIN\n    DELETE FROM {inner_table_name_quoted} WHERE {full_where};\nEND");

    format!(
        "CREATE TRIGGER {trigger_name} INSTEAD OF DELETE ON {table_name_quoted} FOR EACH ROW {trigger_body}"
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
    let view_stmts =
        parse_generated_sql(&dialect, &view_sql, "Failed to parse generated RLS view SQL")?;
    statements.extend(view_stmts);

    // Generate INSERT trigger
    let insert_sql = generate_insert_trigger_sql(table, schema, options);
    let insert_stmts = parse_generated_sql(
        &dialect,
        &insert_sql,
        "Failed to parse generated RLS INSERT trigger SQL",
    )?;
    statements.extend(insert_stmts);

    // Generate UPDATE trigger
    let update_sql = generate_update_trigger_sql(table, schema, options);
    let update_stmts = parse_generated_sql(
        &dialect,
        &update_sql,
        "Failed to parse generated RLS UPDATE trigger SQL",
    )?;
    statements.extend(update_stmts);

    // Generate DELETE trigger
    let delete_sql = generate_delete_trigger_sql(table, schema, options);
    let delete_stmts = parse_generated_sql(
        &dialect,
        &delete_sql,
        "Failed to parse generated RLS DELETE trigger SQL",
    )?;
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
    let view_stmts =
        parse_generated_sql(&dialect, &view_sql, "Failed to parse generated read-only RLS view")?;
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
    renamed.name = append_suffix(&renamed.name, suffix);

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
    let audit_table_name_quoted = quote_identifier(audit_table_name);
    format!(
        r"CREATE TABLE IF NOT EXISTS {audit_table_name_quoted} (
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
        .map(|col| {
            format!(
                "{} || quote({})",
                sql_string_literal(&format!("{col}=")),
                prefixed_quoted_identifier(prefix, col)
            )
        })
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
    let table_name_quoted = quote_identifier(table_name);
    let where_clause = if pk_columns.is_empty() {
        // No PK - check all rows (will be slow but correct)
        "1=1".to_string()
    } else {
        pk_columns
            .iter()
            .map(|col| {
                format!(
                    "{table_name_quoted}.{} = {}",
                    quote_identifier(col),
                    prefixed_quoted_identifier(prefix, col)
                )
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    };

    format!("EXISTS (SELECT 1 FROM {table_name_quoted} WHERE {where_clause})")
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
    let trigger_name = quote_identifier(&format!("{inner_table_name}_rls_monitor_{operation}"));
    let inner_table_name_quoted = quote_identifier(inner_table_name);
    let audit_table_name_quoted = quote_identifier(audit_table_name);
    let table_name_literal = sql_string_literal(table_name);
    let policy_name_literal = sql_string_literal(&format!("{op_upper} policy"));
    let severity_literal = sql_string_literal(severity);
    let details_literal =
        sql_string_literal(&format!("Row {past_participle} backing table but not visible through RLS view"));

    let abort_clause = if strict_mode {
        let message = format!(
            "{RLS_VALIDATION_ERROR}: row violates row-level security policy for table '{table_name}'"
        );
        format!(
            r"
        SELECT RAISE(ABORT, {});",
            sql_string_literal(&message)
        )
    } else {
        String::new()
    };

    format!(
        r"CREATE TRIGGER {trigger_name}
AFTER {op_upper} ON {inner_table_name_quoted}
FOR EACH ROW
BEGIN
    -- Check if {operation}d row is visible through RLS view
    INSERT INTO {audit_table_name_quoted} (
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
        {table_name_literal},
        'rls_policy_violation',
        {row_identifier},
        {policy_name_literal},
        datetime('now'),
        {severity_literal},
        {details_literal},
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
    let table_name_quoted = quote_identifier(table_name);
    let inner_table_name_quoted = quote_identifier(inner_table_name);
    let validation_view_name = quote_identifier(&format!("{inner_table_name}_violations"));
    let column_list =
        columns.iter().map(|column| quote_identifier(column)).collect::<Vec<_>>().join(", ");

    // Build the WHERE clause to match rows by primary key
    let pk_match = if pk_columns.is_empty() {
        // No PK - this is rare but we'll use all columns (inefficient but correct)
        columns
            .iter()
            .map(|col| {
                let col_quoted = quote_identifier(col);
                format!("{inner_table_name_quoted}.{col_quoted} = {table_name_quoted}.{col_quoted}")
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    } else {
        pk_columns
            .iter()
            .map(|col| {
                let col_quoted = quote_identifier(col);
                format!("{inner_table_name_quoted}.{col_quoted} = {table_name_quoted}.{col_quoted}")
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    };

    format!(
        r"CREATE VIEW {validation_view_name} AS
SELECT {column_list}
FROM {inner_table_name_quoted}
WHERE NOT EXISTS (
    SELECT 1
    FROM {table_name_quoted}
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
        let error_context = format!("Failed to parse generated RLS {operation} monitoring trigger");
        let stmts = parse_generated_sql(&dialect, &monitor_sql, &error_context)?;
        statements.extend(stmts);
    }

    // Generate validation view
    let validation_view_sql =
        generate_validation_view_sql(table_name, &inner_table_name, &all_columns, &pk_columns);
    let view_stmts = parse_generated_sql(
        &dialect,
        &validation_view_sql,
        "Failed to parse generated RLS validation view",
    )?;
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

    parse_single_generated_sql(&dialect, &sql, "Failed to parse generated RLS audit table SQL")
}

#[cfg(test)]
mod tests {
    use sql_traits::{structs::ParserDB, traits::DatabaseLike};
    use sqlparser::{
        ast::{
            Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgOperator,
            FunctionArgumentList, FunctionArguments, Ident, JoinConstraint, JoinOperator,
            ObjectName, ObjectNamePart, Query, SetExpr, Statement, TableFactor,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        SubqueryTransformContext, extract_current_setting_name, extract_string_literal,
        filter_policies, generate_delete_trigger_sql, generate_insert_trigger_sql,
        generate_readonly_rls_statements, generate_rls_audit_table, generate_rls_statements,
        generate_rls_validation_statements, generate_rls_view_sql, generate_update_trigger_sql,
        rename_table_for_rls, transform_expr, transform_expr_for_update_check,
        transform_join_operator_for_subquery, transform_query, transform_table_factor_for_subquery,
        validate_session_variables, validate_table_policies,
    };
    use crate::{
        prelude::{Pg2SqliteOptions, TranslationOptions},
        traits::translation_options::SessionVariableMapping,
    };

    fn parse_statements(sql: &str) -> Vec<Statement> {
        Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse")
    }

    fn parse_query(sql: &str) -> Query {
        let stmt = parse_statements(sql).remove(0);
        let Statement::Query(query) = stmt else {
            panic!("expected query");
        };
        *query
    }

    fn parse_expr(sql: &str) -> Expr {
        Parser::new(&PostgreSqlDialect {})
            .try_with_sql(sql)
            .expect("sql should parse")
            .parse_expr()
            .expect("expression should parse")
    }

    fn schema_from_sql(sql: &str) -> ParserDB {
        ParserDB::from_statements(parse_statements(sql), "test".to_string())
            .expect("schema should build")
    }

    #[test]
    fn extract_helpers_cover_string_literal_and_current_setting_edge_paths() {
        assert_eq!(
            extract_string_literal(&Expr::Value(sqlparser::ast::ValueWithSpan::from(
                sqlparser::ast::Value::SingleQuotedString("x".to_string()),
            )))
            .as_deref(),
            Some("x")
        );
        assert!(
            extract_string_literal(&Expr::Value(sqlparser::ast::ValueWithSpan::from(
                sqlparser::ast::Value::Boolean(true),
            )))
            .is_none()
        );
        assert!(extract_string_literal(&parse_expr("other_col")).is_none());

        let not_setting = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("other"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::None,
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        assert!(extract_current_setting_name(&not_setting).is_none());

        let invalid_arg = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("current_setting"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Named {
                    name: Ident::new("x"),
                    arg: FunctionArgExpr::Wildcard,
                    operator: FunctionArgOperator::RightArrow,
                }],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        assert!(extract_current_setting_name(&invalid_arg).is_none());

        let named_expr = Function {
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Named {
                    name: Ident::new("setting"),
                    arg: FunctionArgExpr::Expr(Expr::Value(sqlparser::ast::ValueWithSpan::from(
                        sqlparser::ast::Value::SingleQuotedString("app.user_id".to_string()),
                    ))),
                    operator: FunctionArgOperator::RightArrow,
                }],
                clauses: vec![],
            }),
            ..invalid_arg
        };
        assert_eq!(extract_current_setting_name(&named_expr).as_deref(), Some("app.user_id"));

        let current_setting_no_args = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("current_setting"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::None,
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        assert!(extract_current_setting_name(&current_setting_no_args).is_none());
    }

    #[test]
    fn transform_expr_covers_current_user_cast_and_coalesce_paths() {
        let schema = schema_from_sql(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, title TEXT);",
        );
        let table = schema.table(None, "docs").expect("table should exist");
        let options = Pg2SqliteOptions::default().with_session_variable(
            crate::traits::translation_options::SessionVariableMapping::current_user("sqlite_user"),
        );

        let transformed_current_user = transform_expr(
            &Expr::Identifier(Ident::new("current_user")),
            &options,
            table,
            &schema,
            Some("NEW"),
            None,
        );
        assert_eq!(transformed_current_user.to_string(), "sqlite_user()");

        let transformed_cast = transform_expr(
            &parse_expr("owner_id::INT"),
            &options,
            table,
            &schema,
            Some("NEW"),
            None,
        );
        assert!(transformed_cast.to_string().contains("NEW.owner_id"));

        let renamed = transform_expr(
            &parse_expr("docs.owner_id"),
            &options,
            table,
            &schema,
            None,
            Some(("docs", "docs_rls")),
        );
        assert_eq!(renamed.to_string(), "docs_rls.owner_id");

        let coalesced = transform_expr_for_update_check(
            &parse_expr("docs.owner_id"),
            &options,
            table,
            &schema,
            Some(("docs", "docs_rls")),
        );
        assert!(coalesced.to_string().contains("COALESCE"));

        let coalesced_identifier = transform_expr_for_update_check(
            &parse_expr("owner_id"),
            &options,
            table,
            &schema,
            None,
        );
        assert!(coalesced_identifier.to_string().contains("COALESCE"));
    }

    #[test]
    fn transform_query_and_subquery_helpers_cover_projection_and_join_paths() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id > 0);
            CREATE TABLE teams(id INTEGER PRIMARY KEY, owner_id INTEGER);
            "#,
        );
        let table = schema.table(None, "docs").expect("table should exist");
        let options = Pg2SqliteOptions::default();

        let mut wildcard_query = parse_query("SELECT * FROM docs");
        let SetExpr::Select(select) = wildcard_query.body.as_mut() else {
            panic!("expected select");
        };
        select.qualify = Some(parse_expr("id > 0"));
        let transformed = transform_query(
            &wildcard_query,
            &options,
            table,
            &schema,
            Some("NEW"),
            Some(("docs", "docs_rls")),
        );
        assert!(transformed.to_string().contains("QUALIFY"));

        let context = SubqueryTransformContext {
            options: &options,
            table,
            schema: &schema,
            prefix: Some("NEW"),
            outer_table: Some(("docs", "docs_rls")),
            rls_suffix: options.get_rls_table_suffix(),
        };

        let mut rename_pairs = Vec::new();
        let mut already_suffixed = TableFactor::Table {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("docs_rls"))]),
            alias: None,
            args: None,
            with_hints: vec![],
            version: None,
            with_ordinality: false,
            partitions: vec![],
            json_path: None,
            sample: None,
            index_hints: vec![],
        };
        transform_table_factor_for_subquery(&mut already_suffixed, &context, &mut rename_pairs);
        assert_eq!(rename_pairs, vec![("docs_rls".to_string(), "docs_rls".to_string())]);

        let mut table_function =
            TableFactor::TableFunction { expr: parse_expr("generate_series(1, 2)"), alias: None };
        transform_table_factor_for_subquery(&mut table_function, &context, &mut rename_pairs);
        assert!(matches!(table_function, TableFactor::TableFunction { .. }));

        let on_constraint = JoinConstraint::On(parse_expr("docs.id = teams.id"));
        let mut join_variants = vec![
            JoinOperator::Join(on_constraint.clone()),
            JoinOperator::Inner(on_constraint.clone()),
            JoinOperator::Left(on_constraint.clone()),
            JoinOperator::LeftOuter(on_constraint.clone()),
            JoinOperator::Right(on_constraint.clone()),
            JoinOperator::RightOuter(on_constraint.clone()),
            JoinOperator::FullOuter(on_constraint.clone()),
            JoinOperator::CrossJoin(on_constraint.clone()),
            JoinOperator::Semi(on_constraint.clone()),
            JoinOperator::LeftSemi(on_constraint.clone()),
            JoinOperator::RightSemi(on_constraint.clone()),
            JoinOperator::Anti(on_constraint.clone()),
            JoinOperator::LeftAnti(on_constraint.clone()),
            JoinOperator::RightAnti(on_constraint.clone()),
            JoinOperator::StraightJoin(on_constraint.clone()),
        ];
        for join_op in &mut join_variants {
            transform_join_operator_for_subquery(join_op, &context, &rename_pairs);
        }

        let mut as_of = JoinOperator::AsOf {
            constraint: on_constraint.clone(),
            match_condition: parse_expr("docs.id > teams.id"),
        };
        transform_join_operator_for_subquery(&mut as_of, &context, &rename_pairs);
        assert!(matches!(as_of, JoinOperator::AsOf { .. }));

        let mut cross_apply = JoinOperator::CrossApply;
        transform_join_operator_for_subquery(&mut cross_apply, &context, &rename_pairs);
        let mut outer_apply = JoinOperator::OuterApply;
        transform_join_operator_for_subquery(&mut outer_apply, &context, &rename_pairs);
    }

    #[test]
    fn generate_rls_view_sql_without_select_policies_omits_where_clause() {
        let schema =
            schema_from_sql("CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER);");
        let table = schema.table(None, "docs").expect("table should exist");
        let options = Pg2SqliteOptions::default();
        let view_sql =
            generate_rls_view_sql(table, &schema, &options).expect("view sql should build");
        assert!(!view_sql.contains(" WHERE "));
    }

    #[test]
    fn transform_expr_explicitly_covers_coalesce_and_rename_strategy_variants() {
        let schema = schema_from_sql(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, body TEXT);",
        );
        let table = schema.table(None, "docs").expect("table should exist");
        let options = Pg2SqliteOptions::default();

        let coalesced_ident = transform_expr_for_update_check(
            &parse_expr("owner_id"),
            &options,
            table,
            &schema,
            None,
        );
        assert!(coalesced_ident.to_string().starts_with("COALESCE("));

        let renamed_compound = transform_expr(
            &parse_expr("docs.owner_id"),
            &options,
            table,
            &schema,
            None,
            Some(("docs", "docs_inner")),
        );
        assert_eq!(renamed_compound.to_string(), "docs_inner.owner_id");

        let coalesced_compound = transform_expr_for_update_check(
            &parse_expr("docs.owner_id"),
            &options,
            table,
            &schema,
            Some(("docs", "docs_inner")),
        );
        assert_eq!(coalesced_compound.to_string(), "COALESCE(NEW.owner_id, OLD.owner_id)");
    }

    #[test]
    fn transform_expr_identifier_and_compound_strategy_branches_are_exercised() {
        let schema = schema_from_sql(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, body TEXT);",
        );
        let table = schema.table(None, "docs").expect("table should exist");
        let options = Pg2SqliteOptions::default();

        let coalesced_identifier = transform_expr_for_update_check(
            &Expr::Identifier(Ident::new("owner_id")),
            &options,
            table,
            &schema,
            None,
        );
        assert_eq!(coalesced_identifier.to_string(), "COALESCE(NEW.owner_id, OLD.owner_id)");

        let renamed_compound = transform_expr(
            &Expr::CompoundIdentifier(vec![Ident::new("docs"), Ident::new("owner_id")]),
            &options,
            table,
            &schema,
            None,
            Some(("docs", "docs_inner")),
        );
        assert_eq!(renamed_compound.to_string(), "docs_inner.owner_id");

        let coalesced_compound = transform_expr_for_update_check(
            &Expr::CompoundIdentifier(vec![Ident::new("docs"), Ident::new("owner_id")]),
            &options,
            table,
            &schema,
            Some(("docs", "docs_inner")),
        );
        assert_eq!(coalesced_compound.to_string(), "COALESCE(NEW.owner_id, OLD.owner_id)");
    }

    #[test]
    fn validate_session_variable_and_policy_paths_cover_error_and_success_cases() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id = current_setting('app.user_id')::INT);
            "#,
        );
        let table = schema.table(None, "docs").expect("table should exist");

        let missing = Pg2SqliteOptions::default();
        let err = validate_session_variables(
            &parse_expr("owner_id = current_setting('app.user_id')::INT"),
            &missing,
            "docs",
            "docs_select",
        )
        .expect_err("missing current_setting mapping should error");
        assert!(err.to_string().contains("current_setting('app.user_id')"));

        let err = validate_session_variables(
            &parse_expr("current_user = 'alice'"),
            &missing,
            "docs",
            "docs_select",
        )
        .expect_err("missing current_user mapping should error");
        assert!(err.to_string().contains("current_user"));

        let mapped = Pg2SqliteOptions::default()
            .with_session_variable(SessionVariableMapping::current_setting(
                "app.user_id",
                "sqlite_user_id",
            ))
            .with_session_variable(SessionVariableMapping::current_user("sqlite_user"));
        validate_session_variables(
            &parse_expr("owner_id = current_setting('app.user_id')::INT"),
            &mapped,
            "docs",
            "docs_select",
        )
        .expect("mapped current_setting should pass");
        validate_session_variables(
            &parse_expr("current_user = 'alice'"),
            &mapped,
            "docs",
            "docs_select",
        )
        .expect("mapped current_user should pass");

        validate_table_policies(table, &schema, &mapped).expect("policy validation should pass");
    }

    #[test]
    fn query_and_trigger_generation_helpers_cover_policy_paths() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, body TEXT);
            CREATE TABLE teams(id INTEGER PRIMARY KEY, owner_id INTEGER);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id > 0);
            CREATE POLICY docs_insert ON docs FOR INSERT WITH CHECK (owner_id > 0);
            CREATE POLICY docs_update ON docs FOR UPDATE USING (owner_id > 0) WITH CHECK (owner_id > 0);
            CREATE POLICY docs_delete ON docs FOR DELETE USING (owner_id > 0);
            "#,
        );
        let table = schema.table(None, "docs").expect("table should exist");
        let options = Pg2SqliteOptions::default()
            .with_rls_audit_table_name("rls_audit")
            .with_strict_rls_validation();

        let select_policies =
            filter_policies(table, &schema, &[sqlparser::ast::CreatePolicyCommand::Select]);
        assert_eq!(select_policies.len(), 1);

        let transformed_query = transform_query(
            &parse_query(
                "SELECT docs.owner_id + 1 AS owner_plus \
                 FROM docs INNER JOIN teams ON docs.owner_id = teams.owner_id \
                 WHERE docs.owner_id > 0 \
                 GROUP BY docs.owner_id \
                 HAVING docs.owner_id > 1 \
                 QUALIFY docs.owner_id > 2",
            ),
            &options,
            table,
            &schema,
            Some("NEW"),
            Some(("docs", "docs_rls")),
        );
        let transformed_sql = transformed_query.to_string();
        assert!(transformed_sql.contains("NEW.owner_id"));
        assert!(transformed_sql.contains("QUALIFY"));

        let insert_trigger_sql = generate_insert_trigger_sql(table, &schema, &options);
        assert!(insert_trigger_sql.contains("docs_insert_trigger"));
        assert!(insert_trigger_sql.contains("RAISE(ABORT"));

        let update_trigger_sql = generate_update_trigger_sql(table, &schema, &options);
        assert!(update_trigger_sql.contains("docs_update_trigger"));
        assert!(update_trigger_sql.contains("COALESCE(NEW.owner_id, OLD.owner_id)"));

        let delete_trigger_sql = generate_delete_trigger_sql(table, &schema, &options);
        assert!(delete_trigger_sql.contains("docs_delete_trigger"));
    }

    #[test]
    fn rls_statement_generation_paths_cover_readonly_and_validation_helpers() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, body TEXT);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id > 0);
            CREATE POLICY docs_insert ON docs FOR INSERT WITH CHECK (owner_id > 0);
            CREATE POLICY docs_update ON docs FOR UPDATE USING (owner_id > 0) WITH CHECK (owner_id > 0);
            CREATE POLICY docs_delete ON docs FOR DELETE USING (owner_id > 0);
            "#,
        );
        let table = schema.table(None, "docs").expect("table should exist");

        let missing_audit = Pg2SqliteOptions::default();
        let err = generate_rls_statements(table, &schema, &missing_audit)
            .expect_err("missing audit table should error");
        assert!(err.to_string().contains("RLS audit table name"));
        let err = generate_readonly_rls_statements(table, &schema, &missing_audit)
            .expect_err("missing audit table should error");
        assert!(err.to_string().contains("RLS audit table name"));

        let options = Pg2SqliteOptions::default()
            .with_rls_audit_table_name("rls_audit")
            .with_strict_rls_validation();
        let statements = generate_rls_statements(table, &schema, &options)
            .expect("full RLS statements should build");
        assert!(!statements.is_empty());
        assert!(
            statements
                .iter()
                .any(|stmt| stmt.to_string().contains("CREATE TRIGGER docs_insert_trigger"))
        );

        let readonly = generate_readonly_rls_statements(table, &schema, &options)
            .expect("readonly RLS should build");
        assert!(!readonly.is_empty());
        assert!(!readonly.iter().any(|stmt| stmt.to_string().contains("docs_insert_trigger")));

        let validation = generate_rls_validation_statements(table, &schema, &options, "rls_audit")
            .expect("validation statements should build");
        assert!(!validation.is_empty());
        assert!(
            validation
                .iter()
                .any(|stmt| stmt.to_string().contains("CREATE VIEW docs_rls_violations"))
        );

        let audit_table =
            generate_rls_audit_table("rls_audit").expect("audit table SQL should parse");
        assert!(audit_table.to_string().contains("CREATE TABLE"));

        let create_table_stmt =
            parse_statements("CREATE TABLE docs(id INTEGER PRIMARY KEY)").remove(0);
        let Statement::CreateTable(create_table) = create_table_stmt else {
            panic!("expected create table");
        };
        let renamed = rename_table_for_rls(&create_table, &options, &schema);
        assert!(renamed.name.to_string().ends_with("_rls"));
    }

    #[test]
    fn rls_generation_supports_quoted_identifiers() {
        let schema = schema_from_sql(
            r#"
            CREATE TABLE "Order Items"("doc id" INTEGER PRIMARY KEY, "owner id" INTEGER, "body text" TEXT);
            ALTER TABLE "Order Items" ENABLE ROW LEVEL SECURITY;
            CREATE POLICY order_items_select ON "Order Items" FOR SELECT USING ("owner id" > 0);
            CREATE POLICY order_items_insert ON "Order Items" FOR INSERT WITH CHECK ("owner id" > 0);
            CREATE POLICY order_items_update ON "Order Items" FOR UPDATE USING ("owner id" > 0) WITH CHECK ("owner id" > 0);
            CREATE POLICY order_items_delete ON "Order Items" FOR DELETE USING ("owner id" > 0);
            "#,
        );
        let table = schema.table(None, "Order Items").expect("quoted table should exist");
        let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");

        let statements = generate_rls_statements(table, &schema, &options)
            .expect("quoted identifiers in RLS SQL should translate");
        assert!(
            statements.iter().any(|stmt| stmt.to_string().contains("CREATE VIEW")),
            "expected generated RLS view"
        );
    }
}
