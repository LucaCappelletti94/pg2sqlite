//! Implementation of the [`ReverseTranslator`] trait for the
//! `Insert` type.

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    Assignment, AssignmentTarget, ConflictTarget, DoUpdate, Expr, Ident, Insert, ObjectName,
    ObjectNamePart, OnConflict, OnConflictAction, OnInsert, SqliteOnConflict, TableObject,
};

use crate::{
    errors::Error,
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

/// Get the table name from a TableObject.
fn get_table_name(table: &TableObject) -> Option<String> {
    match table {
        TableObject::TableName(name) => Some(name.to_string()),
        TableObject::TableFunction(_) => None,
    }
}

/// Look up primary key columns for a table from the schema.
/// Returns an error if the table is not found.
fn get_primary_key_columns(schema: &ParserDB, table_name: &str) -> Result<Vec<String>, Error> {
    schema
        .table(None, table_name)
        .map(|table| {
            table.primary_key_columns(schema).map(|c| c.column_name().to_string()).collect()
        })
        .ok_or_else(|| Error::TableNotFoundInSchema { table_name: table_name.to_string() })
}

/// Build ON CONFLICT DO UPDATE SET clause for all non-PK columns.
/// Returns an error if the insert columns don't include all PK columns.
fn build_upsert_on_conflict(
    table_name: &str,
    pk_columns: &[String],
    insert_columns: &[Ident],
) -> Result<OnInsert, Error> {
    // Validate that all PK columns are present in insert_columns
    let has_missing_pk = pk_columns
        .iter()
        .any(|pk| !insert_columns.iter().any(|ic| ic.value.eq_ignore_ascii_case(pk)));

    if has_missing_pk {
        return Err(Error::MissingPrimaryKeyInUpsert {
            table_name: table_name.to_string(),
            pk_columns: pk_columns.to_vec(),
            insert_columns: insert_columns.iter().map(|c| c.value.clone()).collect(),
        });
    }

    // Build conflict target from primary key columns
    let conflict_target =
        ConflictTarget::Columns(pk_columns.iter().map(|c| Ident::new(c.clone())).collect());

    // Build SET assignments for non-PK columns
    let assignments: Vec<Assignment> = insert_columns
        .iter()
        .filter(|col| !pk_columns.iter().any(|pk| pk.eq_ignore_ascii_case(&col.value)))
        .map(|col| {
            Assignment {
                target: AssignmentTarget::ColumnName(ObjectName(vec![ObjectNamePart::Identifier(
                    col.clone(),
                )])),
                value: Expr::CompoundIdentifier(vec![Ident::new("EXCLUDED"), col.clone()]),
            }
        })
        .collect();

    Ok(OnInsert::OnConflict(OnConflict {
        conflict_target: Some(conflict_target),
        action: OnConflictAction::DoUpdate(DoUpdate { assignments, selection: None }),
    }))
}

impl ReverseTranslator for Insert {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Insert;

    #[allow(clippy::too_many_lines)]
    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        // Reverse translate the source (VALUES or SELECT)
        let source = self
            .source
            .as_ref()
            .map(|q| q.reverse_translate(schema, options))
            .transpose()?
            .map(Box::new);

        // Reverse translate partitioned expressions if present
        let partitioned = self
            .partitioned
            .as_ref()
            .map(|exprs| {
                exprs
                    .iter()
                    .map(|e| e.reverse_translate(schema, options))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        // Reverse translate RETURNING clause if present
        let returning = self
            .returning
            .as_ref()
            .map(|items| {
                items
                    .iter()
                    .map(|item| reverse_translate_select_item(item, schema, options))
                    .collect::<Result<Vec<_>, Error>>()
            })
            .transpose()?;

        // Reverse translate assignments if present
        let assignments = self
            .assignments
            .iter()
            .map(|assignment| {
                Ok(sqlparser::ast::Assignment {
                    target: assignment.target.clone(),
                    value: assignment.value.reverse_translate(schema, options)?,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        let mut insert = Insert {
            insert_token: self.insert_token.clone(),
            optimizer_hint: self.optimizer_hint.clone(),
            or: None, // Will be set below based on conversion
            ignore: self.ignore,
            into: self.into,
            table: self.table.clone(),
            table_alias: self.table_alias.clone(),
            columns: self.columns.clone(),
            overwrite: self.overwrite,
            source,
            assignments,
            partitioned,
            after_columns: self.after_columns.clone(),
            has_table_keyword: self.has_table_keyword,
            on: self.on.clone(),
            returning,
            replace_into: self.replace_into,
            priority: self.priority,
            insert_alias: self.insert_alias.clone(),
            settings: self.settings.clone(),
            format_clause: self.format_clause.clone(),
            multi_table_insert_type: self.multi_table_insert_type.clone(),
            multi_table_into_clauses: self.multi_table_into_clauses.clone(),
            multi_table_when_clauses: self.multi_table_when_clauses.clone(),
            multi_table_else_clause: self.multi_table_else_clause.clone(),
        };

        // Handle SQLite's INSERT OR IGNORE/REPLACE → PostgreSQL ON CONFLICT
        if let Some(or_clause) = &self.or {
            match or_clause {
                SqliteOnConflict::Ignore => {
                    // INSERT OR IGNORE → INSERT ... ON CONFLICT DO NOTHING
                    insert.on = Some(OnInsert::OnConflict(OnConflict {
                        conflict_target: None,
                        action: OnConflictAction::DoNothing,
                    }));
                }
                SqliteOnConflict::Replace => {
                    // INSERT OR REPLACE → INSERT ... ON CONFLICT (pk) DO UPDATE SET ...
                    // Look up the primary key from the schema
                    let table_name = get_table_name(&self.table).ok_or_else(|| {
                        Error::UnsupportedSQLiteFeature(
                            "INSERT OR REPLACE with table function is not supported".to_string(),
                        )
                    })?;
                    let pk_columns = get_primary_key_columns(schema, &table_name)?;
                    insert.on =
                        Some(build_upsert_on_conflict(&table_name, &pk_columns, &self.columns)?);
                }
                SqliteOnConflict::Rollback | SqliteOnConflict::Abort | SqliteOnConflict::Fail => {
                    // These don't have direct PostgreSQL equivalents
                    // Leave on as-is (from above)
                }
            }
        }

        // Reverse translate ON CONFLICT expressions if present (from original
        // statement)
        if let Some(OnInsert::OnConflict(on_conflict)) = &self.on
            && let OnConflictAction::DoUpdate(do_update) = &on_conflict.action
        {
            // Reverse translate the SET clause expressions
            let translated_assignments = do_update
                .assignments
                .iter()
                .map(|assignment| {
                    Ok(sqlparser::ast::Assignment {
                        target: assignment.target.clone(),
                        value: assignment.value.reverse_translate(schema, options)?,
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?;

            // Reverse translate the WHERE clause if present
            let translated_selection = do_update
                .selection
                .as_ref()
                .map(|expr| expr.reverse_translate(schema, options))
                .transpose()?;

            insert.on = Some(OnInsert::OnConflict(OnConflict {
                conflict_target: on_conflict.conflict_target.clone(),
                action: OnConflictAction::DoUpdate(sqlparser::ast::DoUpdate {
                    assignments: translated_assignments,
                    selection: translated_selection,
                }),
            }));
        }

        Ok(insert)
    }
}

fn reverse_translate_select_item(
    item: &sqlparser::ast::SelectItem,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::SelectItem, Error> {
    Ok(match item {
        sqlparser::ast::SelectItem::UnnamedExpr(expr) => {
            sqlparser::ast::SelectItem::UnnamedExpr(expr.reverse_translate(schema, options)?)
        }
        sqlparser::ast::SelectItem::ExprWithAlias { expr, alias } => {
            sqlparser::ast::SelectItem::ExprWithAlias {
                expr: expr.reverse_translate(schema, options)?,
                alias: alias.clone(),
            }
        }
        other => other.clone(),
    })
}
