//! Implementation of the [`Translator`] trait for the
//! `Statement` type.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike},
};
use sqlparser::ast::{BinaryOperator, Expr, ObjectType, Statement, UnaryOperator};

use crate::{
    errors::Error,
    impls::{
        object_name::{
            normalize_schema_qualified_object_name_for_sqlite, sqlite_unqualified_object_name,
            table_with_implicit_public_lookup,
        },
        translator_impls::{
            condition_injection::inject_condition_into_dml_statement,
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
    inject_condition_into_dml_statement(stmt, condition)
}

fn or_chain(expressions: &[Expr]) -> Option<Expr> {
    let mut iter = expressions.iter().cloned();
    let first = iter.next()?;
    Some(iter.fold(first, |acc, expr| {
        Expr::BinaryOp { left: Box::new(acc), op: BinaryOperator::Or, right: Box::new(expr) }
    }))
}

fn negate(expr: Expr) -> Expr {
    Expr::UnaryOp { op: UnaryOperator::Not, expr: Box::new(Expr::Nested(Box::new(expr))) }
}

fn append_guarded_statements(
    output: &mut Vec<Statement>,
    branch_statements: &[Statement],
    guard: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    for stmt in branch_statements {
        let mut translated_stmts = stmt.translate(schema, options)?;
        for translated_stmt in &mut translated_stmts {
            inject_condition(translated_stmt, guard.clone())?;
            output.push(translated_stmt.clone());
        }
    }
    Ok(())
}

fn append_translated_create_trigger_statements(
    statements: &mut Vec<Statement>,
    create_trigger: &sqlparser::ast::CreateTrigger,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    for (maybe_drop_trigger, translated_trigger) in create_trigger.translate(schema, options)? {
        if let Some(drop_trigger) = maybe_drop_trigger {
            statements.push(drop_trigger.into());
        }
        statements.push(translated_trigger.into());
    }
    Ok(())
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
        | Statement::ShowTables { .. }
        | Statement::Analyze { .. }
        | Statement::Deallocate { .. }
        | Statement::Prepare { .. }
        | Statement::Execute { .. }
        | Statement::CreateFunction(_)
        | Statement::CreateExtension(_)
        | Statement::CreatePolicy(_)
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
        // User/Role/Schema management (no SQLite equivalent). Postgres
        // `ALTER USER` now parses as `Statement::AlterRole` on apache
        // main (upstream #2374) and is handled by the ALTER ROLE arm
        // below; the `AlterUser` variant here covers dialects such as
        // Snowflake that still emit it.
        | Statement::AlterUser(_)
        | Statement::CreateSchema { .. }
        | Statement::CreateDatabase { .. }
        | Statement::AlterSchema(_)
        | Statement::AlterSession { .. }
        // PostgreSQL-specific types/domains/sequences
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
        // PostgreSQL/Snowflake collation DDL (sqlparser 0.62)
        | Statement::CreateCollation(_)
        | Statement::AlterCollation(_)
        // PostgreSQL ALTER FUNCTION / ALTER AGGREGATE (sqlparser 0.62)
        | Statement::AlterFunction(_)
        // Locking and SHOW variants (sqlparser 0.62)
        | Statement::Lock { .. }
        | Statement::ShowCatalogs { .. }
        | Statement::ShowProcessList { .. }
        // MSSQL WAITFOR (sqlparser 0.62)
        | Statement::WaitFor { .. }
        // Snowflake PUT (post-0.62.0 upstream main)
        | Statement::Put { .. }
        // Post-0.62.0 upstream main: Postgres text-search DDL plus
        // Snowflake file-format and warehouse DDL.
        | Statement::CreateTextSearch(_)
        | Statement::AlterTextSearch(_)
        | Statement::CreateFileFormat { .. }
        | Statement::CreateWarehouse(_)
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

    if create_table.has_row_level_security(schema) {
        validate_table_policies(create_table, schema, options)?;
        let rls_statements = generate_rls_statements(create_table, schema, options)?;
        return build_create_table_statements(create_table, schema, options, Some(rls_statements));
    }

    build_create_table_statements(create_table, schema, options, None)
}

fn translate_create_table_for_role(
    create_table: &sqlparser::ast::CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Vec<Statement>>, Error> {
    let Some(role) = resolve_session_role(schema, options) else {
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
        let rls_statements = if is_readonly {
            generate_readonly_rls_statements(table, schema, options)?
        } else {
            generate_rls_statements(table, schema, options)?
        };
        let statements =
            build_create_table_statements(create_table, schema, options, Some(rls_statements))?;
        return Ok(Some(statements));
    }

    if is_readonly {
        let statements = build_create_table_statements(create_table, schema, options, None)?;
        return Ok(Some(statements));
    }

    Ok(None)
}

fn build_create_table_statements(
    create_table: &sqlparser::ast::CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
    rls_statements: Option<Vec<Statement>>,
) -> Result<Vec<Statement>, Error> {
    let mut statements = if let Some(rls_statements) = rls_statements {
        let translated_table = create_table.translate(schema, options)?;
        let inner_table = rename_table_for_rls(&translated_table, options, schema);
        let mut statements = vec![Statement::CreateTable(inner_table)];
        statements.extend(rls_statements);
        statements
    } else {
        vec![Statement::CreateTable(create_table.translate(schema, options)?)]
    };

    append_vec0_statements_if_needed(&mut statements, create_table, schema, options)?;
    Ok(statements)
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

enum RoleTableAccess {
    Allow,
    Deny,
}

fn resolve_session_role<'a>(
    schema: &'a ParserDB,
    options: &Pg2SqliteOptions,
) -> Option<&'a <ParserDB as DatabaseLike>::Role> {
    let role_name = options.get_session_user_role()?;
    schema.role(role_name)
}

fn role_access_for_object_name(
    table_name: &sqlparser::ast::ObjectName,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<RoleTableAccess, Error> {
    let Some(role) = resolve_session_role(schema, options) else {
        return Ok(RoleTableAccess::Allow);
    };

    let Some(table) = table_with_implicit_public_lookup(schema, table_name)? else {
        return Err(Error::TableNotFoundInSchema { table_name: table_name.to_string() });
    };

    if table.can_select(role, schema) {
        Ok(RoleTableAccess::Allow)
    } else {
        Ok(RoleTableAccess::Deny)
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
                translate_create_table(create_table, schema, options)?
            }
            Self::CreateIndex(create_index) => {
                match role_access_for_object_name(&create_index.table_name, schema, options)? {
                    RoleTableAccess::Allow => create_index.translate(schema, options)?,
                    RoleTableAccess::Deny => Vec::new(),
                }
            }

            Self::CreateTrigger(create_trigger) => {
                if let RoleTableAccess::Deny =
                    role_access_for_object_name(&create_trigger.table_name, schema, options)?
                {
                    return Ok(Vec::new());
                }

                let mut statements = vec![];
                append_translated_create_trigger_statements(
                    &mut statements,
                    create_trigger,
                    schema,
                    options,
                )?;
                statements
            }
            Self::Insert(insert) => vec![insert.translate(schema, options)?.into()],
            Self::CreateView(create_view) => {
                let mut stmts: Vec<Statement> = Vec::new();
                if create_view.or_replace {
                    // SQLite has no CREATE OR REPLACE VIEW, so emit DROP VIEW IF EXISTS first
                    stmts.push(Statement::Drop {
                        object_type: ObjectType::View,
                        if_exists: true,
                        names: vec![sqlite_unqualified_object_name(&create_view.name)],
                        cascade: false,
                        restrict: false,
                        purge: false,
                        temporary: false,
                        table: None,
                    });
                }
                stmts.push(create_view.translate(schema, options)?.into());
                stmts
            }
            Self::Update(update) => vec![Statement::Update(update.translate(schema, options)?)],
            Self::Delete(delete) => vec![delete.translate(schema, options)?],
            Self::Query(query) => {
                vec![Statement::Query(Box::new(query.translate(schema, options)?))]
            }
            Self::If(if_stmt) => {
                let Some(if_condition) = &if_stmt.if_block.condition else {
                    return Ok(Vec::new());
                };

                let translated_if_condition = if_condition.translate(schema, options)?;
                let mut statements = Vec::new();
                append_guarded_statements(
                    &mut statements,
                    if_stmt.if_block.statements(),
                    &translated_if_condition,
                    schema,
                    options,
                )?;

                let mut prior_conditions = vec![translated_if_condition];

                for elseif_block in &if_stmt.elseif_blocks {
                    let Some(elseif_condition) = &elseif_block.condition else {
                        continue;
                    };
                    let translated_elseif_condition = elseif_condition.translate(schema, options)?;
                    let guard = if let Some(prior_any) = or_chain(&prior_conditions) {
                        Expr::BinaryOp {
                            left: Box::new(negate(prior_any)),
                            op: BinaryOperator::And,
                            right: Box::new(translated_elseif_condition.clone()),
                        }
                    } else {
                        translated_elseif_condition.clone()
                    };

                    append_guarded_statements(
                        &mut statements,
                        elseif_block.statements(),
                        &guard,
                        schema,
                        options,
                    )?;
                    prior_conditions.push(translated_elseif_condition);
                }

                if let Some(else_block) = &if_stmt.else_block
                    && let Some(prior_any) = or_chain(&prior_conditions)
                {
                    append_guarded_statements(
                        &mut statements,
                        else_block.statements(),
                        &negate(prior_any),
                        schema,
                        options,
                    )?;
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
                        let normalized_names = names
                            .iter()
                            .map(|name| normalize_schema_qualified_object_name_for_sqlite(schema, name))
                            .collect::<Result<Vec<_>, _>>()?;
                        vec![Statement::Drop {
                            object_type: *object_type,
                            if_exists: *if_exists,
                            names: normalized_names,
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
                    trigger_name: normalize_schema_qualified_object_name_for_sqlite(
                        schema,
                        &drop_trigger.trigger_name,
                    )?,
                    table_name: None, // SQLite doesn't use ON table_name
                    option: None,     // SQLite doesn't support CASCADE/RESTRICT
                })]
            }
            Statement::LISTEN { .. } => {
                crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
                    construct: "LISTEN",
                    reason: "SQLite has no pub/sub channel, so the statement was dropped.",
                });
                Vec::new()
            }
            Statement::UNLISTEN { .. } => {
                crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
                    construct: "UNLISTEN",
                    reason: "SQLite has no pub/sub channel, so the statement was dropped.",
                });
                Vec::new()
            }
            Statement::NOTIFY { .. } => {
                crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
                    construct: "NOTIFY",
                    reason: "SQLite has no pub/sub channel, so the statement was dropped.",
                });
                Vec::new()
            }
            Statement::CreateType { .. } => {
                crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
                    construct: "CREATE TYPE",
                    reason: "SQLite has no composite or enum types, so the type definition was dropped.",
                });
                Vec::new()
            }
            Statement::CreateDomain(_) => {
                crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
                    construct: "CREATE DOMAIN",
                    reason: "SQLite has no domain types, so the domain definition was dropped.",
                });
                Vec::new()
            }
            Statement::CreateServer(_) => {
                crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
                    construct: "CREATE SERVER",
                    reason: "SQLite has no foreign-data-wrapper layer, so the server definition was dropped.",
                });
                Vec::new()
            }
            Statement::CreateRole(_) => {
                crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
                    construct: "CREATE ROLE",
                    reason: "SQLite has no role or access-control layer, so the role definition was dropped.",
                });
                Vec::new()
            }
            Statement::CreateUser(_) => {
                crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
                    construct: "CREATE USER",
                    reason: "SQLite has no user or access-control layer, so the user definition was dropped.",
                });
                Vec::new()
            }
            Statement::AlterRole { .. } => {
                crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
                    construct: "ALTER ROLE",
                    reason: "SQLite has no role or access-control layer, so the role change was dropped.",
                });
                Vec::new()
            }
            Statement::Grant(_) => {
                crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
                    construct: "GRANT",
                    reason: "SQLite has no privilege model, so the GRANT statement was dropped.",
                });
                Vec::new()
            }
            Statement::Revoke(_) => {
                crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
                    construct: "REVOKE",
                    reason: "SQLite has no privilege model, so the REVOKE statement was dropped.",
                });
                Vec::new()
            }
            unsupported_statement_patterns!() => Vec::new(),
        })
    }
}

#[cfg(all(test, feature = "std"))]
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
    fn statement_if_with_else_translates_into_guarded_statements() {
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
        let translated = if_stmt.translate(&schema, &options).expect("IF/ELSE should translate");
        assert_eq!(translated.len(), 2, "expected two statements for IF/ELSE");

        let rendered = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
        let upper = rendered.to_uppercase();
        assert!(upper.contains("DELETE"), "expected DELETE output, got: {rendered}");
        assert!(upper.contains("TRUE"), "expected IF guard, got: {rendered}");
        assert!(upper.contains("NOT"), "expected negated ELSE guard, got: {rendered}");
    }

    #[test]
    fn statement_if_with_elseif_and_else_translates_all_branches() {
        let if_stmt = Parser::parse_sql(
            &PostgreSqlDialect {},
            "IF FALSE THEN DELETE FROM users WHERE id = 1; \
             ELSEIF TRUE THEN DELETE FROM users WHERE id = 2; \
             ELSE DELETE FROM users WHERE id = 3; END IF;",
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

        let schema = ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap();
        let options = Pg2SqliteOptions::default();
        let translated =
            if_stmt.translate(&schema, &options).expect("IF/ELSIF/ELSE should translate");
        assert_eq!(translated.len(), 3, "expected one statement per branch");

        let rendered = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
        let upper = rendered.to_uppercase();
        assert!(rendered.contains("id = 1"), "missing IF branch statement: {rendered}");
        assert!(rendered.contains("id = 2"), "missing ELSIF branch statement: {rendered}");
        assert!(rendered.contains("id = 3"), "missing ELSE branch statement: {rendered}");
        assert!(
            upper.contains("NOT FALSE") || upper.contains("NOT (FALSE)"),
            "missing branch exclusivity guard for ELSIF/ELSE: {rendered}"
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

    #[test]
    fn role_filtered_create_index_is_skipped_for_non_selectable_table() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(
                &PostgreSqlDialect {},
                r#"
                CREATE ROLE app_user;
                CREATE TABLE private_docs(id INTEGER PRIMARY KEY, title TEXT);
                CREATE INDEX private_docs_title_idx ON private_docs(title);
                "#,
            )
            .expect("schema SQL should parse"),
            "test".to_string(),
        )
        .expect("schema should build");

        let options = Pg2SqliteOptions::default().with_session_user_role("app_user");
        let index_stmt = Parser::parse_sql(
            &PostgreSqlDialect {},
            "CREATE INDEX private_docs_title_idx ON private_docs(title);",
        )
        .expect("index SQL should parse")
        .remove(0);

        let translated =
            index_stmt.translate(&schema, &options).expect("translation should succeed");
        assert!(translated.is_empty(), "non-selectable table index should be filtered out");
    }

    #[test]
    fn role_filtered_create_trigger_is_skipped_for_non_selectable_table() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(
                &PostgreSqlDialect {},
                r#"
                CREATE ROLE app_user;
                CREATE TABLE private_docs(id INTEGER PRIMARY KEY, title TEXT);
                CREATE FUNCTION private_docs_trigger_fn() RETURNS trigger AS $$
                BEGIN
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER private_docs_ai
                AFTER INSERT ON private_docs
                FOR EACH ROW
                EXECUTE FUNCTION private_docs_trigger_fn();
                "#,
            )
            .expect("schema SQL should parse"),
            "test".to_string(),
        )
        .expect("schema should build");

        let options = Pg2SqliteOptions::default().with_session_user_role("app_user");
        let trigger_stmt = Parser::parse_sql(
            &PostgreSqlDialect {},
            "CREATE TRIGGER private_docs_ai AFTER INSERT ON private_docs FOR EACH ROW EXECUTE FUNCTION private_docs_trigger_fn();",
        )
        .expect("trigger SQL should parse")
        .remove(0);

        let translated =
            trigger_stmt.translate(&schema, &options).expect("translation should succeed");
        assert!(translated.is_empty(), "non-selectable table trigger should be filtered out");
    }

    #[test]
    fn maintenance_before_insert_or_update_trigger_splits_into_two_sqlite_triggers() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(
                &PostgreSqlDialect {},
                r#"
                CREATE TABLE brands(id INTEGER PRIMARY KEY, name TEXT, edited_at TEXT);
                CREATE FUNCTION set_brands_edited_at() RETURNS trigger AS $$
                BEGIN
                    NEW.edited_at = CURRENT_TIMESTAMP;
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                "#,
            )
            .expect("schema SQL should parse"),
            "test".to_string(),
        )
        .expect("schema should build");

        let trigger_stmt = Parser::parse_sql(
            &PostgreSqlDialect {},
            r#"
            CREATE TRIGGER trigger_upsert_brands_edited_at
            BEFORE INSERT OR UPDATE ON brands
            FOR EACH ROW
            EXECUTE FUNCTION set_brands_edited_at();
            "#,
        )
        .expect("trigger SQL should parse")
        .remove(0);

        let translated = trigger_stmt
            .translate(&schema, &Pg2SqliteOptions::default())
            .expect("translation should succeed");

        let all_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
        assert!(
            all_sql.contains(
                "CREATE TRIGGER trigger_upsert_brands_edited_at BEFORE UPDATE OF id, name ON brands"
            ),
            "split update trigger should preserve BEFORE UPDATE semantics: {all_sql}"
        );
        assert!(
            all_sql.contains(
                "CREATE TRIGGER trigger_upsert_brands_edited_at_pg2sqlite_insert AFTER INSERT ON brands"
            ),
            "split insert trigger should be translated to AFTER INSERT: {all_sql}"
        );
    }

    #[test]
    fn role_filtered_create_index_errors_when_table_lookup_fails() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, "CREATE ROLE app_user;")
                .expect("schema SQL should parse"),
            "test".to_string(),
        )
        .expect("schema should build");

        let options = Pg2SqliteOptions::default().with_session_user_role("app_user");
        let index_stmt = Parser::parse_sql(
            &PostgreSqlDialect {},
            "CREATE INDEX missing_docs_title_idx ON missing_docs(title);",
        )
        .expect("index SQL should parse")
        .remove(0);

        let err = index_stmt.translate(&schema, &options).expect_err("translation should fail");
        assert!(
            matches!(&err, crate::errors::Error::TableNotFoundInSchema { table_name } if table_name == "missing_docs"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn role_filtered_create_trigger_errors_when_table_lookup_fails() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, "CREATE ROLE app_user;")
                .expect("schema SQL should parse"),
            "test".to_string(),
        )
        .expect("schema should build");

        let options = Pg2SqliteOptions::default().with_session_user_role("app_user");
        let trigger_stmt = Parser::parse_sql(
            &PostgreSqlDialect {},
            "CREATE TRIGGER missing_docs_ai AFTER INSERT ON missing_docs FOR EACH ROW EXECUTE FUNCTION docs_trigger_fn();",
        )
        .expect("trigger SQL should parse")
        .remove(0);

        let err = trigger_stmt.translate(&schema, &options).expect_err("translation should fail");
        assert!(
            matches!(&err, crate::errors::Error::TableNotFoundInSchema { table_name } if table_name == "missing_docs"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn unknown_session_role_does_not_filter_create_index_or_trigger() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(
                &PostgreSqlDialect {},
                r#"
                CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT);
                CREATE FUNCTION docs_trigger_fn() RETURNS trigger AS $$
                BEGIN
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE INDEX docs_title_idx ON docs(title);
                CREATE TRIGGER docs_ai
                AFTER INSERT ON docs
                FOR EACH ROW
                EXECUTE FUNCTION docs_trigger_fn();
                "#,
            )
            .expect("schema SQL should parse"),
            "test".to_string(),
        )
        .expect("schema should build");

        let options = Pg2SqliteOptions::default().with_session_user_role("missing_role");

        let index_stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, "CREATE INDEX docs_title_idx ON docs(title);")
                .expect("index SQL should parse")
                .remove(0);
        let trigger_stmt = Parser::parse_sql(
            &PostgreSqlDialect {},
            "CREATE TRIGGER docs_ai AFTER INSERT ON docs FOR EACH ROW EXECUTE FUNCTION docs_trigger_fn();",
        )
        .expect("trigger SQL should parse")
        .remove(0);

        let translated_index =
            index_stmt.translate(&schema, &options).expect("index translation should succeed");
        assert!(
            !translated_index.is_empty(),
            "unknown role should not filter CREATE INDEX statements"
        );

        let translated_trigger =
            trigger_stmt.translate(&schema, &options).expect("trigger translation should succeed");
        assert!(
            !translated_trigger.is_empty(),
            "unknown role should not filter CREATE TRIGGER statements"
        );
    }

    #[test]
    fn role_filtered_create_index_with_public_schema_is_allowed_when_selectable() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(
                &PostgreSqlDialect {},
                r#"
                CREATE ROLE app_user;
                CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT);
                GRANT SELECT ON docs TO app_user;
                "#,
            )
            .expect("schema SQL should parse"),
            "test".to_string(),
        )
        .expect("schema should build");

        let options = Pg2SqliteOptions::default().with_session_user_role("app_user");
        let index_stmt = Parser::parse_sql(
            &PostgreSqlDialect {},
            "CREATE INDEX docs_title_idx ON public.docs(title);",
        )
        .expect("index SQL should parse")
        .remove(0);

        let translated =
            index_stmt.translate(&schema, &options).expect("translation should succeed");
        assert!(!translated.is_empty(), "public-qualified table should be considered selectable");
    }

    #[test]
    fn role_filtered_create_index_with_non_public_schema_errors() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(
                &PostgreSqlDialect {},
                r#"
                CREATE ROLE app_user;
                CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT);
                GRANT SELECT ON docs TO app_user;
                "#,
            )
            .expect("schema SQL should parse"),
            "test".to_string(),
        )
        .expect("schema should build");

        let options = Pg2SqliteOptions::default().with_session_user_role("app_user");
        let index_stmt = Parser::parse_sql(
            &PostgreSqlDialect {},
            "CREATE INDEX docs_title_idx ON my_custom_app.docs(title);",
        )
        .expect("index SQL should parse")
        .remove(0);

        let err = index_stmt.translate(&schema, &options).expect_err("translation should fail");
        assert!(
            err.to_string().contains("Unsupported schema-qualified object name")
                && err.to_string().contains("does not resolve"),
            "unexpected error: {err}"
        );
    }
}
