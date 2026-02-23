//! Implementation of the [`ReverseTranslator`] trait for the
//! `Statement` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    AccessExpr, Delete, Expr, FromTable, FunctionArg, FunctionArgExpr, FunctionArguments,
    GroupByExpr, Insert, LimitClause, ObjectName, Query, Select, SelectItem, SetExpr, Statement,
    TableFactor, TableObject, TableWithJoins, Update, UpdateTableFromKind, Values, WindowType,
};

use crate::{
    errors::Error,
    prelude::{Pg2SqliteOptions, ReverseTranslator},
    traits::TranslationOptions,
};

/// Check if a table name ends with the RLS suffix.
fn is_rls_table(name: &ObjectName, options: &Pg2SqliteOptions) -> bool {
    let table_name = name.to_string();
    let suffix = options.get_rls_table_suffix();
    table_name.ends_with(suffix)
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

/// Check all table references in a FROM clause for RLS tables.
fn check_from_clause_for_rls(
    from: &[TableWithJoins],
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    for table_with_joins in from {
        check_table_factor_for_rls(&table_with_joins.relation, options)?;
        for join in &table_with_joins.joins {
            check_table_factor_for_rls(&join.relation, options)?;
        }
    }
    Ok(())
}

/// Check a FromTable enum for RLS tables.
fn check_from_table_for_rls(from: &FromTable, options: &Pg2SqliteOptions) -> Result<(), Error> {
    match from {
        FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => {
            check_from_clause_for_rls(tables, options)
        }
    }
}

/// Check an UpdateTableFromKind for RLS tables.
fn check_update_from_for_rls(
    from: &UpdateTableFromKind,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match from {
        UpdateTableFromKind::BeforeSet(tables) | UpdateTableFromKind::AfterSet(tables) => {
            check_from_clause_for_rls(tables, options)
        }
    }
}

/// Check a table factor for RLS table access.
fn check_table_factor_for_rls(
    factor: &TableFactor,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match factor {
        TableFactor::Table { name, .. } => check_table_for_rls(name, options),
        TableFactor::Derived { subquery, .. } => check_query_for_rls(subquery, options),
        TableFactor::NestedJoin { table_with_joins, .. } => {
            check_table_factor_for_rls(&table_with_joins.relation, options)?;
            for join in &table_with_joins.joins {
                check_table_factor_for_rls(&join.relation, options)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn check_expr_pair_for_rls(
    left: &Expr,
    right: &Expr,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(left, options)?;
    check_expr_for_rls(right, options)
}

fn check_expr_slice_for_rls(exprs: &[Expr], options: &Pg2SqliteOptions) -> Result<(), Error> {
    for expr in exprs {
        check_expr_for_rls(expr, options)?;
    }
    Ok(())
}

fn check_case_expr_for_rls(
    operand: Option<&Expr>,
    conditions: &[sqlparser::ast::CaseWhen],
    else_result: Option<&Expr>,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    if let Some(operand) = operand {
        check_expr_for_rls(operand, options)?;
    }
    for condition in conditions {
        check_expr_pair_for_rls(&condition.condition, &condition.result, options)?;
    }
    if let Some(else_result) = else_result {
        check_expr_for_rls(else_result, options)?;
    }
    Ok(())
}

fn check_trim_expr_for_rls(
    expr: &Expr,
    trim_what: Option<&Expr>,
    trim_characters: Option<&[Expr]>,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(expr, options)?;
    if let Some(trim_what) = trim_what {
        check_expr_for_rls(trim_what, options)?;
    }
    if let Some(trim_characters) = trim_characters {
        check_expr_slice_for_rls(trim_characters, options)?;
    }
    Ok(())
}

fn check_substring_expr_for_rls(
    expr: &Expr,
    substring_from: Option<&Expr>,
    substring_for: Option<&Expr>,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(expr, options)?;
    if let Some(substring_from) = substring_from {
        check_expr_for_rls(substring_from, options)?;
    }
    if let Some(substring_for) = substring_for {
        check_expr_for_rls(substring_for, options)?;
    }
    Ok(())
}

fn check_overlay_expr_for_rls(
    expr: &Expr,
    overlay_what: &Expr,
    overlay_from: &Expr,
    overlay_for: Option<&Expr>,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(expr, options)?;
    check_expr_for_rls(overlay_what, options)?;
    check_expr_for_rls(overlay_from, options)?;
    if let Some(overlay_for) = overlay_for {
        check_expr_for_rls(overlay_for, options)?;
    }
    Ok(())
}

fn check_compound_access_for_rls(
    root: &Expr,
    access_chain: &[AccessExpr],
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(root, options)?;
    for access in access_chain {
        if let AccessExpr::Dot(nested) = access {
            check_expr_for_rls(nested, options)?;
        }
    }
    Ok(())
}

/// Check an expression tree for RLS table references in subqueries.
fn check_expr_for_rls(expr: &Expr, options: &Pg2SqliteOptions) -> Result<(), Error> {
    match expr {
        Expr::Subquery(query) => check_query_for_rls(query, options),
        Expr::Exists { subquery, .. } => check_query_for_rls(subquery, options),
        Expr::InSubquery { expr, subquery, .. } => {
            check_expr_for_rls(expr, options)?;
            check_query_for_rls(subquery, options)
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::AnyOp { left, right, .. }
        | Expr::AllOp { left, right, .. } => check_expr_pair_for_rls(left, right, options),
        Expr::Function(func) => check_function_for_rls(func, options),
        Expr::UnaryOp { expr, .. }
        | Expr::Cast { expr, .. }
        | Expr::Extract { expr, .. }
        | Expr::Ceil { expr, .. }
        | Expr::Floor { expr, .. } => check_expr_for_rls(expr, options),
        Expr::Nested(inner) => check_expr_for_rls(inner, options),
        Expr::AtTimeZone { timestamp, time_zone } => {
            check_expr_pair_for_rls(timestamp, time_zone, options)
        }
        Expr::Like { expr, pattern, .. }
        | Expr::ILike { expr, pattern, .. }
        | Expr::SimilarTo { expr, pattern, .. }
        | Expr::RLike { expr, pattern, .. } => check_expr_pair_for_rls(expr, pattern, options),
        Expr::Tuple(exprs) => check_expr_slice_for_rls(exprs, options),
        Expr::Array(array) => check_expr_slice_for_rls(&array.elem, options),
        Expr::Case { operand, conditions, else_result, .. } => {
            check_case_expr_for_rls(operand.as_deref(), conditions, else_result.as_deref(), options)
        }
        Expr::Between { expr, low, high, .. } => {
            check_expr_for_rls(expr, options)?;
            check_expr_pair_for_rls(low, high, options)
        }
        Expr::InList { expr, list, .. } => {
            check_expr_for_rls(expr, options)?;
            check_expr_slice_for_rls(list, options)
        }
        Expr::Trim { expr, trim_what, trim_characters, .. } => {
            check_trim_expr_for_rls(expr, trim_what.as_deref(), trim_characters.as_deref(), options)
        }
        Expr::Position { expr, r#in } => check_expr_pair_for_rls(expr, r#in, options),
        Expr::Substring { expr, substring_from, substring_for, .. } => {
            check_substring_expr_for_rls(
                expr,
                substring_from.as_deref(),
                substring_for.as_deref(),
                options,
            )
        }
        Expr::Overlay { expr, overlay_what, overlay_from, overlay_for } => {
            check_overlay_expr_for_rls(
                expr,
                overlay_what,
                overlay_from,
                overlay_for.as_deref(),
                options,
            )
        }
        Expr::Prefixed { value, .. } | Expr::Collate { expr: value, .. } => {
            check_expr_for_rls(value, options)
        }
        Expr::Interval(interval) => check_expr_for_rls(&interval.value, options),
        Expr::CompoundFieldAccess { root, access_chain } => {
            check_compound_access_for_rls(root, access_chain, options)
        }
        _ => Ok(()),
    }
}

/// Check a select item for RLS table references in subqueries.
fn check_select_item_for_rls(item: &SelectItem, options: &Pg2SqliteOptions) -> Result<(), Error> {
    match item {
        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
            check_expr_for_rls(expr, options)
        }
        _ => Ok(()),
    }
}

fn check_select_for_rls(select: &Select, options: &Pg2SqliteOptions) -> Result<(), Error> {
    check_from_clause_for_rls(&select.from, options)?;

    if let Some(selection) = &select.selection {
        check_expr_for_rls(selection, options)?;
    }

    if let Some(having) = &select.having {
        check_expr_for_rls(having, options)?;
    }

    for item in &select.projection {
        check_select_item_for_rls(item, options)?;
    }

    if let GroupByExpr::Expressions(exprs, _) = &select.group_by {
        for expr in exprs {
            check_expr_for_rls(expr, options)?;
        }
    }

    if let Some(qualify) = &select.qualify {
        check_expr_for_rls(qualify, options)?;
    }

    Ok(())
}

fn check_values_for_rls(values: &Values, options: &Pg2SqliteOptions) -> Result<(), Error> {
    for row in &values.rows {
        for expr in row {
            check_expr_for_rls(expr, options)?;
        }
    }
    Ok(())
}

fn check_set_expr_for_rls(set_expr: &SetExpr, options: &Pg2SqliteOptions) -> Result<(), Error> {
    match set_expr {
        SetExpr::Select(select) => check_select_for_rls(select, options),
        SetExpr::Query(query) => check_query_for_rls(query, options),
        SetExpr::SetOperation { left, right, .. } => {
            check_set_expr_for_rls(left, options)?;
            check_set_expr_for_rls(right, options)
        }
        SetExpr::Insert(stmt) => {
            if let Statement::Insert(insert) = stmt {
                check_insert_for_rls(insert, options)?;
            }
            Ok(())
        }
        SetExpr::Update(stmt) => {
            if let Statement::Update(update) = stmt {
                check_update_for_rls(update, options)?;
            }
            Ok(())
        }
        SetExpr::Delete(stmt) => {
            if let Statement::Delete(delete) = stmt {
                check_delete_for_rls(delete, options)?;
            }
            Ok(())
        }
        SetExpr::Values(values) => check_values_for_rls(values, options),
        SetExpr::Table(_) | SetExpr::Merge(_) => Ok(()),
    }
}

fn check_limit_clause_for_rls(
    limit_clause: &LimitClause,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match limit_clause {
        LimitClause::LimitOffset { limit, offset, limit_by } => {
            if let Some(limit) = limit {
                check_expr_for_rls(limit, options)?;
            }
            if let Some(offset) = offset {
                check_expr_for_rls(&offset.value, options)?;
            }
            for expr in limit_by {
                check_expr_for_rls(expr, options)?;
            }
            Ok(())
        }
        LimitClause::OffsetCommaLimit { offset, limit } => {
            check_expr_for_rls(offset, options)?;
            check_expr_for_rls(limit, options)
        }
    }
}

fn check_query_for_rls(query: &Query, options: &Pg2SqliteOptions) -> Result<(), Error> {
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            check_query_for_rls(&cte.query, options)?;
        }
    }
    check_set_expr_for_rls(query.body.as_ref(), options)?;

    if let Some(order_by) = &query.order_by
        && let sqlparser::ast::OrderByKind::Expressions(exprs) = &order_by.kind
    {
        for order_expr in exprs {
            check_expr_for_rls(&order_expr.expr, options)?;
        }
    }

    if let Some(limit_clause) = &query.limit_clause {
        check_limit_clause_for_rls(limit_clause, options)?;
    }

    if let Some(fetch) = &query.fetch
        && let Some(quantity) = &fetch.quantity
    {
        check_expr_for_rls(quantity, options)?;
    }

    Ok(())
}

fn check_function_for_rls(
    function: &sqlparser::ast::Function,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    if let FunctionArguments::List(arg_list) = &function.args {
        for arg in &arg_list.args {
            match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))
                | FunctionArg::Named { arg: FunctionArgExpr::Expr(expr), .. }
                | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(expr), .. } => {
                    check_expr_for_rls(expr, options)?;
                }
                _ => {}
            }
        }
    }

    if let Some(filter) = &function.filter {
        check_expr_for_rls(filter, options)?;
    }

    if let Some(over) = &function.over
        && let WindowType::WindowSpec(window_spec) = over
    {
        for expr in &window_spec.partition_by {
            check_expr_for_rls(expr, options)?;
        }
        for order_by_expr in &window_spec.order_by {
            check_expr_for_rls(&order_by_expr.expr, options)?;
        }
    }

    for order_by_expr in &function.within_group {
        check_expr_for_rls(&order_by_expr.expr, options)?;
    }

    Ok(())
}

fn check_insert_for_rls(insert: &Insert, options: &Pg2SqliteOptions) -> Result<(), Error> {
    check_table_object_for_rls(&insert.table, options)?;

    if let Some(source) = &insert.source {
        check_query_for_rls(source, options)?;
    }

    Ok(())
}

fn check_update_for_rls(update: &Update, options: &Pg2SqliteOptions) -> Result<(), Error> {
    check_table_factor_for_rls(&update.table.relation, options)?;
    for join in &update.table.joins {
        check_table_factor_for_rls(&join.relation, options)?;
    }

    if let Some(from) = &update.from {
        check_update_from_for_rls(from, options)?;
    }

    if let Some(selection) = &update.selection {
        check_expr_for_rls(selection, options)?;
    }

    for assignment in &update.assignments {
        check_expr_for_rls(&assignment.value, options)?;
    }

    Ok(())
}

fn check_delete_for_rls(delete: &Delete, options: &Pg2SqliteOptions) -> Result<(), Error> {
    for table_name in &delete.tables {
        check_table_for_rls(table_name, options)?;
    }

    check_from_table_for_rls(&delete.from, options)?;

    if let Some(using) = &delete.using {
        check_from_clause_for_rls(using, options)?;
    }

    if let Some(selection) = &delete.selection {
        check_expr_for_rls(selection, options)?;
    }

    Ok(())
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
            // Non-DML statements are not supported for reverse translation
            other => {
                let debug = format!("{other:?}");
                let variant_name = debug.split(['(', '{', ' ']).next().unwrap_or("Unknown");
                Err(Error::UnsupportedReverseStatement { statement_type: variant_name.to_string() })
            }
        }
    }
}
