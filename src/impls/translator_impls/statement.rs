//! Implementation of the [`Translator`] trait for the
//! `Statement` type.

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike},
};
use sqlparser::ast::{BinaryOperator, Expr, Statement};

use crate::{
    impls::translator_impls::rls::{
        generate_readonly_rls_statements, generate_rls_statements, rename_table_for_rls,
        validate_table_policies,
    },
    prelude::{Pg2SqliteOptions, Translator},
    traits::TranslationOptions,
};

fn inject_condition(stmt: &mut Statement, condition: Expr) {
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
                    _ => unimplemented!("Cannot inject condition into non-SELECT insert source"),
                }
            } else {
                unimplemented!("Cannot inject condition into INSERT without source")
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
        _ => unimplemented!("Cannot inject condition into statement: {:?}", stmt),
    }
}

impl Translator for Statement {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Vec<Statement>;

    #[allow(clippy::too_many_lines)]
    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        Ok(match self {
            Self::CreateTable(create_table) => {
                // Check if we need to filter based on grants to a specific role
                if let Some(role_name) = options.get_session_user_role()
                    && let Some(role) = schema.role(role_name)
                {
                    // Look up the table in the schema to access TableLike methods
                    if let Some(table) =
                        schema.table(create_table.table_schema(), create_table.table_name())
                    {
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
                            let inner_table =
                                rename_table_for_rls(&translated_table, options, schema);

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
                        // Non-RLS writable table: fall through to normal
                        // handling
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

                    statements
                } else {
                    vec![create_table.translate(schema, options)?.into()]
                }
            }
            Self::CreateIndex(create_index) => {
                create_index.translate(schema, options)?.map(Into::into).into_iter().collect()
            }
            Self::CreateFunction(_)
            | Self::CreateExtension(_)
            | Self::CreatePolicy(_)
            | Self::CreateRole(_)
            | Self::Grant(_)
            | Self::Revoke(_) => Vec::new(),
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
            Self::Delete(delete) => vec![delete.translate(schema, options)?],
            Self::If(if_stmt) => {
                if !if_stmt.elseif_blocks.is_empty() || if_stmt.else_block.is_some() {
                    return Err(crate::errors::Error::UnknownPostgresFeature(
                        "IF statements with ELSE/ELSEIF not yet supported".into(),
                    ));
                }

                let condition = if let Some(cond) = &if_stmt.if_block.condition {
                    cond.clone()
                } else {
                    return Ok(Vec::new());
                };

                let mut statements = Vec::new();
                for stmt in if_stmt.if_block.statements() {
                    let mut translated_stmts = stmt.translate(schema, options)?;
                    for translated_stmt in &mut translated_stmts {
                        inject_condition(translated_stmt, condition.clone());
                        statements.push(translated_stmt.clone());
                    }
                }
                statements
            }
            Self::AlterTable(alter_table) => {
                if alter_table.operations.iter().all(|op| {
                    matches!(
                        op,
                        sqlparser::ast::AlterTableOperation::EnableRowLevelSecurity
                            | sqlparser::ast::AlterTableOperation::DisableRowLevelSecurity
                    )
                }) {
                    Vec::new()
                } else {
                    unimplemented!(
                        "Unsupported PostgreSQL statement: `{}` - Parsed as: {alter_table:?}",
                        Statement::AlterTable(alter_table.clone()).to_string()
                    )
                }
            }
            unsupported_statement => {
                unimplemented!(
                    "Unsupported PostgreSQL statement: `{}` - Parsed as: {unsupported_statement:?}",
                    unsupported_statement.to_string()
                )
            }
        })
    }
}
