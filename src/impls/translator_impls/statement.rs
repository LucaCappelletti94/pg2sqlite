//! Implementation of the [`Translator`] trait for the
//! `Statement` type.

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike},
};
use sqlparser::ast::{BinaryOperator, Expr, ObjectType, Statement};

use crate::{
    errors::Error,
    impls::{
        shared_helpers::statement_variant_name,
        translator_impls::{
            rls::{
                generate_readonly_rls_statements, generate_rls_statements, rename_table_for_rls,
                validate_table_policies,
            },
            vector::{generate_vec0_statements, has_vector_columns},
        },
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
            let variant_name = statement_variant_name(stmt);
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "Cannot inject IF condition into statement variant `{variant_name}`",
            )));
        }
    }
    Ok(())
}

fn unsupported_statement_error(stmt: &Statement) -> crate::errors::Error {
    let variant_name = statement_variant_name(stmt);
    crate::errors::Error::UnsupportedSQLiteFeature(format!(
        "Unsupported PostgreSQL statement variant `{variant_name}`"
    ))
}

macro_rules! unsupported_statement_patterns {
    () => {
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
    };
}

fn translate_create_table(
    create_table: &sqlparser::ast::CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, Error> {
    if let Some(role_filtered) = translate_create_table_for_role(create_table, schema, options)? {
        return Ok(role_filtered);
    }

    let mut statements = if create_table.has_row_level_security(schema) {
        validate_table_policies(create_table, schema, options)?;
        let translated_table = create_table.translate(schema, options)?;
        let inner_table = rename_table_for_rls(&translated_table, options, schema);
        let mut statements = vec![Statement::CreateTable(inner_table)];
        statements.extend(generate_rls_statements(create_table, schema, options)?);
        statements
    } else {
        vec![Statement::CreateTable(create_table.translate(schema, options)?)]
    };

    append_vec0_statements_if_needed(&mut statements, create_table, schema, options)?;
    Ok(statements)
}

fn translate_create_table_for_role(
    create_table: &sqlparser::ast::CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Vec<Statement>>, Error> {
    let Some(role_name) = options.get_session_user_role() else {
        return Ok(None);
    };
    let Some(role) = schema.role(role_name) else {
        return Ok(None);
    };
    let Some(table) = schema.table(create_table.table_schema(), create_table.table_name()) else {
        return Ok(None);
    };

    if !table.can_select(role, schema) {
        return Ok(Some(Vec::new()));
    }

    let is_readonly = !table.can_write(role, schema);
    if table.has_row_level_security(schema) {
        validate_table_policies(table, schema, options)?;
        let translated_table = create_table.translate(schema, options)?;
        let inner_table = rename_table_for_rls(&translated_table, options, schema);
        let mut statements = vec![Statement::CreateTable(inner_table)];
        if is_readonly {
            statements.extend(generate_readonly_rls_statements(table, schema, options)?);
        } else {
            statements.extend(generate_rls_statements(table, schema, options)?);
        }
        append_vec0_statements_if_needed(&mut statements, create_table, schema, options)?;
        return Ok(Some(statements));
    }

    if is_readonly {
        let mut statements = vec![Statement::CreateTable(create_table.translate(schema, options)?)];
        append_vec0_statements_if_needed(&mut statements, create_table, schema, options)?;
        return Ok(Some(statements));
    }

    Ok(None)
}

fn append_vec0_statements_if_needed(
    statements: &mut Vec<Statement>,
    create_table: &sqlparser::ast::CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    if has_vector_columns(create_table) {
        statements.extend(generate_vec0_statements(create_table, schema, options)?);
    }
    Ok(())
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
                    _ => {
                        if options.should_fail_on_unsupported_statement() {
                            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                                "Unsupported PostgreSQL DROP object type encountered: {object_type:?}"
                            )));
                        }
                        Vec::new()
                    }
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
            stmt @ unsupported_statement_patterns!() => {
                if options.should_fail_on_unsupported_statement() {
                    return Err(unsupported_statement_error(stmt));
                }
                Vec::new()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{CreateTable, Expr, Statement, Value, ValueWithSpan},
        dialect::{PostgreSqlDialect, SQLiteDialect},
        parser::Parser,
    };

    use super::{inject_condition, translate_create_table_for_role};
    use crate::{
        prelude::{Pg2SqliteOptions, Translator},
        traits::TranslationOptions,
    };

    fn parse_create_table(sql: &str) -> CreateTable {
        let stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse").remove(0);
        let Statement::CreateTable(create_table) = stmt else {
            panic!("expected create table");
        };
        create_table
    }

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

    #[test]
    fn inject_condition_updates_insert_update_and_delete_statements() {
        let condition = Expr::Value(ValueWithSpan {
            value: Value::Boolean(true),
            span: sqlparser::tokenizer::Span::empty(),
        });

        let mut insert = Parser::parse_sql(
            &PostgreSqlDialect {},
            "INSERT INTO logs(id) SELECT id FROM users WHERE active = 1",
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
        inject_condition(&mut insert, condition.clone()).unwrap();
        assert!(insert.to_string().to_uppercase().contains("AND TRUE"), "unexpected SQL: {insert}");

        let mut update =
            Parser::parse_sql(&PostgreSqlDialect {}, "UPDATE users SET active = 0 WHERE id = 1")
                .unwrap()
                .into_iter()
                .next()
                .unwrap();
        inject_condition(&mut update, condition.clone()).unwrap();
        assert!(update.to_string().to_uppercase().contains("AND TRUE"), "unexpected SQL: {update}");

        let mut delete = Parser::parse_sql(&PostgreSqlDialect {}, "DELETE FROM users WHERE id = 1")
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        inject_condition(&mut delete, condition).unwrap();
        assert!(delete.to_string().to_uppercase().contains("AND TRUE"), "unexpected SQL: {delete}");
    }

    #[test]
    fn inject_condition_rejects_insert_without_select_source() {
        let condition = Expr::Value(ValueWithSpan {
            value: Value::Boolean(true),
            span: sqlparser::tokenizer::Span::empty(),
        });

        let mut values_insert =
            Parser::parse_sql(&PostgreSqlDialect {}, "INSERT INTO t(id) VALUES (1)")
                .unwrap()
                .into_iter()
                .next()
                .unwrap();
        let err = inject_condition(&mut values_insert, condition.clone()).unwrap_err();
        assert!(err.to_string().contains("non-SELECT source"), "unexpected error: {err}");

        let mut source_less_insert = values_insert.clone();
        if let Statement::Insert(insert) = &mut source_less_insert {
            insert.source = None;
        } else {
            panic!("expected insert statement");
        }
        let err = inject_condition(&mut source_less_insert, condition).unwrap_err();
        assert!(err.to_string().contains("without source"), "unexpected error: {err}");
    }

    #[test]
    fn statement_if_with_else_is_rejected() {
        let if_stmt = Parser::parse_sql(
            &PostgreSqlDialect {},
            "IF TRUE THEN DELETE FROM users; ELSE DELETE FROM users; END IF;",
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

        let schema = ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap();
        let options = Pg2SqliteOptions::default();
        let err = if_stmt.translate(&schema, &options).unwrap_err();
        assert!(
            err.to_string().contains("IF statements with ELSE/ELSEIF not yet supported"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn translate_create_table_for_role_handles_missing_table_readonly_and_writable_paths() {
        let missing_schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, "CREATE ROLE app_user;")
                .expect("schema SQL should parse"),
            "test".to_string(),
        )
        .expect("schema should build");
        let options = Pg2SqliteOptions::default().with_session_user_role("app_user");
        let missing_table = parse_create_table("CREATE TABLE docs(id INTEGER PRIMARY KEY)");
        let missing =
            translate_create_table_for_role(&missing_table, &missing_schema, &options).unwrap();
        assert!(missing.is_none());

        let readonly_schema_sql = r#"
            CREATE ROLE app_user;
            CREATE TABLE readonly_docs(id INTEGER PRIMARY KEY);
            GRANT SELECT ON readonly_docs TO app_user;
        "#;
        let readonly_schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, readonly_schema_sql)
                .expect("schema SQL should parse"),
            "test".to_string(),
        )
        .expect("schema should build");
        let readonly_table =
            parse_create_table("CREATE TABLE readonly_docs(id INTEGER PRIMARY KEY)");
        let readonly =
            translate_create_table_for_role(&readonly_table, &readonly_schema, &options).unwrap();
        let readonly = readonly.expect("readonly path should return statements");
        assert!(!readonly.is_empty());
        assert!(matches!(readonly[0], Statement::CreateTable(_)));

        let writable_schema_sql = r#"
            CREATE ROLE app_user;
            CREATE TABLE writable_docs(id INTEGER PRIMARY KEY);
            GRANT ALL ON writable_docs TO app_user;
        "#;
        let writable_schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, writable_schema_sql)
                .expect("schema SQL should parse"),
            "test".to_string(),
        )
        .expect("schema should build");
        let writable_table =
            parse_create_table("CREATE TABLE writable_docs(id INTEGER PRIMARY KEY)");
        let writable =
            translate_create_table_for_role(&writable_table, &writable_schema, &options).unwrap();
        assert!(writable.is_none());
    }
}
