//! Implementation of the [`Translator`] trait for the
//! `Statement` type.

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike},
};
use sqlparser::ast::{BinaryOperator, Expr, ObjectType, Statement};

use crate::{
    impls::translator_impls::{
        rls::{
            generate_readonly_rls_statements, generate_rls_statements, rename_table_for_rls,
            validate_table_policies,
        },
        vector::{generate_vec0_statements, has_vector_columns},
    },
    prelude::{Pg2SqliteOptions, Translator},
    traits::TranslationOptions,
};

fn inject_condition(stmt: &mut Statement, condition: Expr) -> Result<(), crate::errors::Error> {
    match stmt {
        Statement::Insert(insert) => {
            if let Some(source) = &mut insert.source {
                match &mut *source.body {
                    sqlparser::ast::SetExpr::Select(select) => {
                        let new_selection = if let Some(existing) = &select.selection {
                            Expr::BinaryOp {
                                left: Box::new(existing.clone()),
                                op: BinaryOperator::And,
                                right: Box::new(condition),
                            }
                        } else {
                            condition
                        };
                        select.selection = Some(new_selection);
                    }
                    _ => {
                        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                            "Cannot inject IF condition into INSERT with non-SELECT source"
                                .to_string(),
                        ));
                    }
                }
            } else {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "Cannot inject IF condition into INSERT without source".to_string(),
                ));
            }
        }
        Statement::Update(update) => {
            let new_selection = if let Some(existing) = &update.selection {
                Expr::BinaryOp {
                    left: Box::new(existing.clone()),
                    op: BinaryOperator::And,
                    right: Box::new(condition),
                }
            } else {
                condition
            };
            update.selection = Some(new_selection);
        }
        Statement::Delete(delete) => {
            let new_selection = if let Some(existing) = &delete.selection {
                Expr::BinaryOp {
                    left: Box::new(existing.clone()),
                    op: BinaryOperator::And,
                    right: Box::new(condition),
                }
            } else {
                condition
            };
            delete.selection = Some(new_selection);
        }
        _ => {
            let debug = format!("{stmt:?}");
            let variant_name = debug.split(['(', '{', ' ']).next().unwrap_or("Unknown");
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "Cannot inject IF condition into statement type: {variant_name}",
            )));
        }
    }
    Ok(())
}

/// Returns `true` for statement variants that have no SQLite equivalent
/// and should be silently filtered out during translation.
#[allow(clippy::too_many_lines)]
fn is_unsupported_statement(stmt: &Statement) -> bool {
    matches!(
        stmt,
        // ALTER TABLE - no direct SQLite equivalent
        Statement::AlterTable(_)
        // Session/variable/maintenance/cursor statements
        | Statement::ShowVariable { .. }
        | Statement::Raise { .. }
        | Statement::Print { .. }
        | Statement::Open { .. }
        | Statement::Close { .. }
        | Statement::Fetch { .. }
        | Statement::Declare { .. }
        | Statement::Use { .. }
        | Statement::Throw { .. }
        | Statement::Load { .. }
        | Statement::Return { .. }
        | Statement::Assert { .. }
        | Statement::While { .. }
        | Statement::ExplainTable { .. }
        | Statement::Explain { .. }
        | Statement::Kill { .. }
        | Statement::LISTEN { .. }
        | Statement::UNLISTEN { .. }
        | Statement::NOTIFY { .. }
        | Statement::ShowTables { .. }
        | Statement::Analyze { .. }
        | Statement::Deallocate { .. }
        | Statement::Prepare { .. }
        | Statement::Execute { .. }
        | Statement::CreateFunction(_)
        | Statement::CreateExtension(_)
        | Statement::CreatePolicy(_)
        | Statement::CreateRole(_)
        | Statement::CreateUser(_)
        | Statement::Grant(_)
        | Statement::Revoke(_)
        | Statement::Set(_)
        | Statement::Pragma { .. }
        | Statement::Call(_)
        | Statement::Reset(_)
        | Statement::Truncate(_)
        | Statement::Directory { .. }
        | Statement::Discard { .. }
        | Statement::ShowViews { .. }
        | Statement::ShowFunctions { .. }
        | Statement::ShowCollation { .. }
        | Statement::ShowCreate { .. }
        | Statement::ShowSchemas { .. }
        | Statement::ShowCharset { .. }
        | Statement::ShowColumns { .. }
        // User/Role/Schema management (no SQLite equivalent)
        | Statement::AlterRole { .. }
        | Statement::AlterUser(_)
        | Statement::CreateSchema { .. }
        | Statement::CreateDatabase { .. }
        | Statement::AlterSchema(_)
        | Statement::AlterSession { .. }
        // PostgreSQL-specific types/domains/sequences
        | Statement::CreateType { .. }
        | Statement::CreateDomain(_)
        | Statement::CreateSequence { .. }
        | Statement::CreateProcedure { .. }
        | Statement::CreateMacro { .. }
        | Statement::AlterType(_)
        | Statement::AlterPolicy(_)
        | Statement::DropPolicy { .. }
        | Statement::DropFunction { .. }
        | Statement::DropExtension { .. }
        | Statement::DropDomain { .. }
        | Statement::DropProcedure { .. }
        // PostgreSQL operators
        | Statement::CreateOperator(_)
        | Statement::CreateOperatorClass(_)
        | Statement::CreateOperatorFamily(_)
        | Statement::AlterOperator(_)
        | Statement::AlterOperatorClass(_)
        | Statement::AlterOperatorFamily(_)
        | Statement::DropOperator { .. }
        | Statement::DropOperatorClass { .. }
        | Statement::DropOperatorFamily { .. }
        // Other database-specific statements
        | Statement::Comment { .. }
        | Statement::Copy { .. }
        | Statement::CopyIntoSnowflake { .. }
        | Statement::Merge(_)
        | Statement::LockTables { .. }
        | Statement::UnlockTables
        | Statement::Flush { .. }
        | Statement::ShowStatus { .. }
        | Statement::ShowVariables { .. }
        | Statement::ShowDatabases { .. }
        | Statement::ShowObjects(_)
        | Statement::RaisError { .. }
        | Statement::Deny { .. }
        | Statement::AlterView { .. }
        | Statement::AlterIndex { .. }
        | Statement::Msck(_)
        | Statement::RenameTable(_)
        // DuckDB-specific
        | Statement::AttachDuckDBDatabase { .. }
        | Statement::DetachDuckDBDatabase { .. }
        // Other vendor-specific
        | Statement::CreateConnector(_)
        | Statement::AlterConnector { .. }
        | Statement::DropConnector { .. }
        | Statement::CreateSecret { .. }
        | Statement::DropSecret { .. }
        | Statement::CreateServer(_)
        | Statement::CreateStage { .. }
        | Statement::Cache { .. }
        | Statement::UNCache { .. }
        | Statement::Install { .. }
        | Statement::List { .. }
        | Statement::Remove { .. }
        | Statement::LoadData { .. }
        | Statement::OptimizeTable { .. }
        | Statement::Unload { .. }
        | Statement::ExportData(_)
        // SQLite-specific statements (no PostgreSQL equivalent needed)
        | Statement::AttachDatabase { .. }
        | Statement::CreateVirtualTable { .. }
        // Case statement (procedural, not supported)
        | Statement::Case(_)
    )
}

fn translate_create_table(
    create_table: &sqlparser::ast::CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, crate::errors::Error> {
    // Check if we need to filter based on grants to a specific role
    if let Some(role_name) = options.get_session_user_role()
        && let Some(role) = schema.role(role_name)
    {
        // Look up the table in the schema to access TableLike methods
        if let Some(table) = schema.table(create_table.table_schema(), create_table.table_name()) {
            // If the role has no SELECT permission, skip this table entirely
            if !table.can_select(role, schema) {
                return Ok(Vec::new());
            }

            // Check if this is a read-only table (SELECT but no write grants)
            let is_readonly = !table.can_write(role, schema);

            // Check if this table has RLS enabled
            if table.has_row_level_security(schema) {
                // Validate all policies have required session variable mappings
                validate_table_policies(table, schema, options)?;

                // Translate the table first
                let translated_table = create_table.translate(schema, options)?;

                // Rename the table to the inner table name
                let inner_table = rename_table_for_rls(&translated_table, options, schema);

                let mut statements = vec![Statement::CreateTable(inner_table)];

                // Generate the view and triggers (or just view for readonly)
                let rls_statements = if is_readonly {
                    generate_readonly_rls_statements(table, schema, options)?
                } else {
                    generate_rls_statements(table, schema, options)?
                };
                statements.extend(rls_statements);

                return Ok(statements);
            } else if is_readonly {
                // Non-RLS readonly table: just create the table as-is
                return Ok(vec![create_table.translate(schema, options)?.into()]);
            }
            // Non-RLS writable table: fall through to normal handling
        }
    }

    // Original logic: Check if this table has RLS enabled
    if create_table.has_row_level_security(schema) {
        // Validate all policies have required session variable mappings
        validate_table_policies(create_table, schema, options)?;

        // Translate the table first
        let translated_table = create_table.translate(schema, options)?;

        // Rename the table to the inner table name
        let inner_table = rename_table_for_rls(&translated_table, options, schema);

        let mut statements = vec![Statement::CreateTable(inner_table)];

        // Generate the view and triggers
        let rls_statements = generate_rls_statements(create_table, schema, options)?;
        statements.extend(rls_statements);

        // Generate vec0 virtual tables for vector columns (using original table)
        if has_vector_columns(create_table) {
            let vec0_statements = generate_vec0_statements(create_table, schema, options)?;
            statements.extend(vec0_statements);
        }

        Ok(statements)
    } else {
        // Translate the table
        let translated_table = create_table.translate(schema, options)?;
        let mut statements = vec![Statement::CreateTable(translated_table)];

        // Generate vec0 virtual tables for vector columns
        if has_vector_columns(create_table) {
            let vec0_statements = generate_vec0_statements(create_table, schema, options)?;
            statements.extend(vec0_statements);
        }

        Ok(statements)
    }
}

impl Translator for Statement {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Vec<Statement>;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        if is_unsupported_statement(self) {
            return Ok(Vec::new());
        }

        Ok(match self {
            Self::CreateTable(create_table) => {
                translate_create_table(create_table, schema, options)?
            }
            Self::CreateIndex(create_index) => create_index.translate(schema, options)?,

            Self::CreateTrigger(create_trigger) => {
                let maybe_translated = create_trigger.translate(schema, options)?;
                let mut statements = vec![];
                if let Some((maybe_drop_trigger, create_trigger)) = maybe_translated {
                    if let Some(drop_trigger) = maybe_drop_trigger {
                        statements.push(drop_trigger.into());
                    }
                    statements.push(create_trigger.into());
                }
                statements
            }
            Self::Insert(insert) => vec![insert.translate(schema, options)?.into()],
            Self::CreateView(create_view) => {
                vec![create_view.translate(schema, options)?.into()]
            }
            Self::Update(update) => vec![Statement::Update(update.translate(schema, options)?)],
            Self::Delete(delete) => vec![delete.translate(schema, options)?],
            Self::Query(query) => {
                vec![Statement::Query(Box::new(query.translate(schema, options)?))]
            }
            Self::If(if_stmt) => {
                if !if_stmt.elseif_blocks.is_empty() || if_stmt.else_block.is_some() {
                    return Err(crate::errors::Error::UnknownPostgresFeature(
                        "IF statements with ELSE/ELSEIF not yet supported".into(),
                    ));
                }

                let condition = if let Some(cond) = &if_stmt.if_block.condition {
                    cond.translate(schema, options)?
                } else {
                    return Ok(Vec::new());
                };

                let mut statements = Vec::new();
                for stmt in if_stmt.if_block.statements() {
                    let mut translated_stmts = stmt.translate(schema, options)?;
                    for translated_stmt in &mut translated_stmts {
                        inject_condition(translated_stmt, condition.clone())?;
                        statements.push(translated_stmt.clone());
                    }
                }
                statements
            }
            // VACUUM is supported by SQLite - pass through
            Self::Vacuum { .. }
            // Transaction control statements - pass through unchanged (SQLite supports these)
            | Self::Commit { .. }
            | Self::Rollback { .. }
            | Self::StartTransaction { .. }
            | Self::Savepoint { .. }
            | Self::ReleaseSavepoint { .. } => vec![self.clone()],
            // DROP TABLE/VIEW/INDEX - translate to SQLite (strip CASCADE/RESTRICT)
            Self::Drop {
                object_type,
                if_exists,
                names,
                ..
            } => {
                match object_type {
                    // SQLite supports these object types
                    ObjectType::Table | ObjectType::View | ObjectType::Index => {
                        vec![Statement::Drop {
                            object_type: *object_type,
                            if_exists: *if_exists,
                            names: names.clone(),
                            cascade: false,  // SQLite doesn't support CASCADE
                            restrict: false, // SQLite doesn't support RESTRICT
                            purge: false,
                            temporary: false,
                            table: None,
                        }]
                    }
                    // Other object types are PostgreSQL-specific, ignore them
                    _ => Vec::new(),
                }
            }
            // DROP TRIGGER - translate to SQLite (strip table name and CASCADE/RESTRICT)
            Self::DropTrigger(drop_trigger) => {
                vec![Statement::DropTrigger(sqlparser::ast::DropTrigger {
                    if_exists: drop_trigger.if_exists,
                    trigger_name: drop_trigger.trigger_name.clone(),
                    table_name: None, // SQLite doesn't use ON table_name
                    option: None,     // SQLite doesn't support CASCADE/RESTRICT
                })]
            }
            // All unsupported variants are handled by the early return above
            _ => Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use sqlparser::{
        ast::{Expr, Value, ValueWithSpan},
        dialect::SQLiteDialect,
        parser::Parser,
    };

    use super::inject_condition;

    #[test]
    fn inject_condition_returns_error_for_unsupported_statement() {
        let mut stmt =
            Parser::parse_sql(&SQLiteDialect {}, "VACUUM;").unwrap().into_iter().next().unwrap();

        let condition = Expr::Value(ValueWithSpan {
            value: Value::Boolean(true),
            span: sqlparser::tokenizer::Span::empty(),
        });

        let result = inject_condition(&mut stmt, condition);
        assert!(result.is_err(), "Expected unsupported statement to return an error");
    }
}
