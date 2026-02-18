//! Implementation of the [`ReverseTranslator`] trait for the
//! `Statement` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    Delete, Expr, FromTable, Insert, ObjectName, Query, Select, SelectItem, SetExpr, Statement,
    TableFactor, TableObject, TableWithJoins, Update, UpdateTableFromKind,
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

/// Check an expression tree for RLS table references in subqueries.
fn check_expr_for_rls(expr: &Expr, options: &Pg2SqliteOptions) -> Result<(), Error> {
    match expr {
        Expr::Subquery(query) => check_query_for_rls(query, options),
        Expr::Exists { subquery, .. } => check_query_for_rls(subquery, options),
        Expr::InSubquery { expr, subquery, .. } => {
            check_expr_for_rls(expr, options)?;
            check_query_for_rls(subquery, options)
        }
        Expr::BinaryOp { left, right, .. } => {
            check_expr_for_rls(left, options)?;
            check_expr_for_rls(right, options)
        }
        Expr::UnaryOp { expr, .. } => check_expr_for_rls(expr, options),
        Expr::Nested(inner) => check_expr_for_rls(inner, options),
        Expr::Case { operand, conditions, else_result, .. } => {
            if let Some(op) = operand {
                check_expr_for_rls(op, options)?;
            }
            for cw in conditions {
                check_expr_for_rls(&cw.condition, options)?;
                check_expr_for_rls(&cw.result, options)?;
            }
            if let Some(el) = else_result {
                check_expr_for_rls(el, options)?;
            }
            Ok(())
        }
        Expr::Between { expr, low, high, .. } => {
            check_expr_for_rls(expr, options)?;
            check_expr_for_rls(low, options)?;
            check_expr_for_rls(high, options)
        }
        Expr::InList { expr, list, .. } => {
            check_expr_for_rls(expr, options)?;
            for item in list {
                check_expr_for_rls(item, options)?;
            }
            Ok(())
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
        SetExpr::Values(_) | SetExpr::Table(_) | SetExpr::Merge(_) => Ok(()),
    }
}

fn check_query_for_rls(query: &Query, options: &Pg2SqliteOptions) -> Result<(), Error> {
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            check_query_for_rls(&cte.query, options)?;
        }
    }
    check_set_expr_for_rls(query.body.as_ref(), options)
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
