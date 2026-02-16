//! Implementation of the [`ReverseTranslator`] trait for the
//! `Statement` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    FromTable, ObjectName, Statement, TableFactor, TableObject, TableWithJoins, UpdateTableFromKind,
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
        TableFactor::Derived { subquery, .. } => {
            // Check subquery for RLS tables
            if let sqlparser::ast::SetExpr::Select(select) = subquery.body.as_ref() {
                check_from_clause_for_rls(&select.from, options)?;
            }
            Ok(())
        }
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
                // Check table for RLS
                check_table_object_for_rls(&insert.table, options)?;

                // Check source query for RLS tables
                if let Some(source) = &insert.source
                    && let sqlparser::ast::SetExpr::Select(select) = source.body.as_ref()
                {
                    check_from_clause_for_rls(&select.from, options)?;
                }

                Ok(Statement::Insert(insert.reverse_translate(schema, options)?))
            }
            Statement::Update(update) => {
                // Check table for RLS (update.table is TableWithJoins)
                check_table_factor_for_rls(&update.table.relation, options)?;

                // Check FROM clause for RLS tables
                if let Some(from) = &update.from {
                    check_update_from_for_rls(from, options)?;
                }

                Ok(Statement::Update(update.reverse_translate(schema, options)?))
            }
            Statement::Delete(delete) => {
                // Check tables for RLS
                for table_name in &delete.tables {
                    check_table_for_rls(table_name, options)?;
                }

                // Check FROM clause for RLS tables
                check_from_table_for_rls(&delete.from, options)?;

                // Check USING clause for RLS tables
                if let Some(using) = &delete.using {
                    check_from_clause_for_rls(using, options)?;
                }

                Ok(Statement::Delete(delete.reverse_translate(schema, options)?))
            }
            Statement::Query(query) => {
                // Check FROM clause for RLS tables
                if let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() {
                    check_from_clause_for_rls(&select.from, options)?;
                }

                Ok(Statement::Query(Box::new(query.reverse_translate(schema, options)?)))
            }
            // Non-DML statements are not supported for reverse translation
            other => {
                Err(Error::UnsupportedReverseStatement {
                    statement_type: format!("{:?}", std::mem::discriminant(other)),
                })
            }
        }
    }
}
