//! Implementation of the [`Translator`] trait for the
//! `Statement` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{BinaryOperator, Expr, Statement};

use crate::prelude::{Pg2SqliteOptions, Translator};

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

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        Ok(match self {
            Self::CreateTable(create_table) => {
                vec![create_table.translate(schema, options)?.into()]
            }
            Self::CreateIndex(create_index) => {
                create_index.translate(schema, options)?.map(Into::into).into_iter().collect()
            }
            Self::CreateFunction(_) | Self::CreateExtension(_) | Self::CreatePolicy(_) => {
                Vec::new()
            }
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
