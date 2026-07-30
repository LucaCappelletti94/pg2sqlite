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
use sqlparser::{
    ast::{
        AlterTable, AlterTableOperation, BinaryOperator, CascadeOption, ColumnDef, ColumnOption,
        CopySource, CopyTarget, Delete, Expr, FromTable, Merge, ObjectType, Statement, TableFactor,
        TableWithJoins, Truncate, TruncateIdentityOption, UnaryOperator,
        helpers::attached_token::AttachedToken,
    },
    dialect::SQLiteDialect,
};

use crate::{
    errors::Error,
    impls::{
        generated_sql::parse_generated_sql,
        object_name::{
            append_suffix, last_ident, normalize_schema_qualified_object_name_for_sqlite,
            quote_identifier, sql_string_literal, sqlite_unqualified_object_name,
            table_has_implicit_public_rls, table_with_implicit_public_lookup,
        },
        placeholder::rewrite_placeholders_for_sqlite,
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
        | Statement::CreateFunction(_)
        | Statement::CreateExtension(_)
        | Statement::CreatePolicy(_)
        | Statement::Set(_)
        | Statement::Pragma { .. }
        | Statement::Call(_)
        | Statement::Reset(_)
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
        | Statement::CreateOperator(_)
        | Statement::CreateOperatorClass(_)
        | Statement::CreateOperatorFamily(_)
        | Statement::AlterOperator(_)
        | Statement::AlterOperatorClass(_)
        | Statement::AlterOperatorFamily(_)
        | Statement::DropOperator { .. }
        | Statement::DropOperatorClass { .. }
        | Statement::DropOperatorFamily { .. }
        | Statement::Comment { .. }
        | Statement::CopyIntoSnowflake { .. }
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
        | Statement::AttachDuckDBDatabase { .. }
        | Statement::DetachDuckDBDatabase { .. }
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
        | Statement::AttachDatabase { .. }
        | Statement::CreateVirtualTable { .. }
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
        let mut statements = build_create_table_statements(create_table, schema, options, None)?;
        append_readonly_deny_triggers(&mut statements, options)?;
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

/// Trigger event per read-only deny trigger, keyed by the verb appended after
/// the configured reserved marker.
const READONLY_DENY_TRIGGERS: [(&str, &str); 3] =
    [("insert", "BEFORE INSERT"), ("update", "BEFORE UPDATE"), ("delete", "BEFORE DELETE")];

/// Appends `RAISE(ABORT)` deny triggers so interactive writes to a read-only
/// non-RLS table fail synchronously at the statement. Names are
/// `<table><marker>_<verb>`, where the marker is
/// [`TranslationOptions::get_readonly_deny_trigger_suffix`]. Errors on a name
/// collision so a clashing schema fails loudly instead of emitting broken SQL.
///
/// Authoritative changeset applies must run with triggers disabled (see
/// [`TranslationOptions::with_session_user_role`]).
fn append_readonly_deny_triggers(
    statements: &mut Vec<Statement>,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    let Some(Statement::CreateTable(create_table)) = statements.first() else {
        return Ok(());
    };
    let sqlite_name = sqlite_unqualified_object_name(&create_table.name);
    let table_ident = last_ident(&sqlite_name)
        .map_or_else(|| sqlite_name.to_string(), |ident| ident.value.clone());

    let marker = options.get_readonly_deny_trigger_suffix();
    let table_name_quoted = quote_identifier(&table_ident);
    let deny_message =
        sql_string_literal(&format!("permission denied: {table_ident} is read-only for this role"));

    let dialect = SQLiteDialect {};
    let mut triggers = Vec::with_capacity(READONLY_DENY_TRIGGERS.len());
    for (verb, event) in READONLY_DENY_TRIGGERS {
        let trigger_name = format!("{table_ident}{marker}_{verb}");
        reject_reserved_name_collision(options, &table_ident, &trigger_name)?;

        let trigger_sql = format!(
            "CREATE TRIGGER {} {event} ON {table_name_quoted} \
             BEGIN SELECT RAISE(ABORT, {deny_message}); END",
            quote_identifier(&trigger_name)
        );
        triggers.extend(parse_generated_sql(
            &dialect,
            &trigger_sql,
            "Failed to parse generated read-only deny trigger SQL",
        )?);
    }

    statements.extend(triggers);
    Ok(())
}

/// Errors when the translation unit already declares a table, index, trigger,
/// or view whose SQLite-unqualified name collides with a reserved deny-trigger
/// name. The declared-name catalog is prewalked from the input statements
/// (see `populate_declared_object_names`) because the translation schema omits
/// index and trigger definitions.
fn reject_reserved_name_collision(
    options: &Pg2SqliteOptions,
    table_name: &str,
    trigger_name: &str,
) -> Result<(), Error> {
    if options.has_declared_object_name(trigger_name) {
        return Err(Error::ReadonlyDenyTriggerNameCollision {
            table_name: table_name.to_string(),
            trigger_name: trigger_name.to_string(),
        });
    }
    Ok(())
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

/// Translates `ALTER TABLE` operations to SQLite.
///
/// PostgreSQL accepts several operations in one statement while SQLite accepts
/// one, so each operation becomes a statement of its own.
fn translate_alter_table(
    alter_table: &AlterTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, Error> {
    let normalized_name =
        normalize_schema_qualified_object_name_for_sqlite(schema, &alter_table.name)?;

    let mut statements = Vec::with_capacity(alter_table.operations.len());
    for operation in &alter_table.operations {
        let Some(translated) =
            translate_alter_table_operation(operation, alter_table, schema, options)?
        else {
            continue;
        };
        statements.push(Statement::AlterTable(AlterTable {
            name: normalized_name.clone(),
            operations: vec![translated],
            ..alter_table.clone()
        }));
    }

    Ok(statements)
}

/// Translates a single `ALTER TABLE` operation.
///
/// SQLite supports `RENAME TO`, `RENAME COLUMN`, `ADD COLUMN` (since 3.1.1),
/// and `DROP COLUMN` (since 3.35.0), all at or below the declared 3.46.0 floor.
///
/// `Ok(None)` means the operation is consumed elsewhere in the pipeline and
/// correctly contributes no `ALTER TABLE` of its own. Anything with no SQLite
/// form is an error rather than a silent drop, because dropping it would change
/// which rows the database accepts or returns.
fn translate_alter_table_operation(
    operation: &AlterTableOperation,
    alter_table: &AlterTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<AlterTableOperation>, Error> {
    match operation {
        AlterTableOperation::RenameTable { .. }
        | AlterTableOperation::RenameColumn { .. }
        | AlterTableOperation::DropColumn { .. } => Ok(Some(operation.clone())),
        AlterTableOperation::AddColumn {
            column_keyword,
            if_not_exists,
            column_def,
            column_position,
        } => {
            reject_unsupported_added_column(column_def, alter_table.name.to_string().as_str())?;
            Ok(Some(AlterTableOperation::AddColumn {
                column_keyword: *column_keyword,
                if_not_exists: *if_not_exists,
                // Route through the same translator the CREATE TABLE path uses so
                // type mapping, STRICT-legal types, and the parenthesisation
                // SQLite requires of a non-literal DEFAULT all apply.
                column_def: column_def.translate(schema, options)?,
                column_position: column_position.clone(),
            }))
        }
        // Row level security is realised as the view and trigger set built from
        // the schema, which already records these, so they carry no ALTER TABLE.
        AlterTableOperation::EnableRowLevelSecurity
        | AlterTableOperation::DisableRowLevelSecurity
        | AlterTableOperation::ForceRowLevelSecurity
        | AlterTableOperation::NoForceRowLevelSecurity => Ok(None),
        other => {
            Err(Error::UnsupportedSQLiteFeature(format!(
                "ALTER TABLE {} {other} has no SQLite equivalent. SQLite can only rename a table or \
             column, add a column, and drop a column, so this operation cannot be applied to an \
             existing table without rebuilding it. Express the intent in the table's CREATE TABLE \
             definition instead.",
                alter_table.name
            )))
        }
    }
}

/// SQLite rejects `ALTER TABLE ... ADD COLUMN` carrying a PRIMARY KEY or UNIQUE
/// constraint regardless of the table's contents, so emitting one would produce
/// SQL that cannot execute. Both are visible in the column definition alone.
///
/// A `NOT NULL` column without a default is deliberately NOT rejected here: it
/// succeeds on an empty table and fails on a populated one, exactly as it does
/// in PostgreSQL, so the runtime error is the faithful outcome.
fn reject_unsupported_added_column(column_def: &ColumnDef, table_name: &str) -> Result<(), Error> {
    for option in &column_def.options {
        let constraint = match &option.option {
            ColumnOption::PrimaryKey(_) => "PRIMARY KEY",
            ColumnOption::Unique(_) => "UNIQUE",
            _ => continue,
        };
        return Err(Error::UnsupportedSQLiteFeature(format!(
            "ALTER TABLE {table_name} ADD COLUMN {} cannot carry a {constraint} constraint: \
             SQLite rejects it because enforcing the constraint would require rewriting the \
             table. Declare the column without it, then add a separate unique index.",
            column_def.name
        )));
    }
    Ok(())
}

/// Translates `TRUNCATE` to one `DELETE FROM` per named table.
///
/// Each table is routed through the `DELETE` translator rather than emitting
/// the statement directly, so row level security rewriting, table renaming, and
/// role access checks apply exactly as they do to a hand-written `DELETE FROM`.
///
/// The identity options need no special handling, which is not the obvious
/// conclusion. This crate emits `AUTOINCREMENT` only for the RLS audit table,
/// so a translated table's primary key is a plain rowid alias with no stored
/// counter. Emptying such a table restarts its identifiers, which is what
/// `RESTART IDENTITY` asks for, so that option matches SQLite exactly. It is
/// `CONTINUE IDENTITY` that cannot be honoured, and since it names PostgreSQL's
/// default it is warned about rather than rejected: refusing it while accepting
/// a bare `TRUNCATE` would reject a statement for spelling out the behaviour it
/// already requested.
fn translate_truncate(
    truncate: &Truncate,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, Error> {
    reject_untranslatable_truncate_options(truncate)?;

    if matches!(truncate.identity, Some(TruncateIdentityOption::Continue)) {
        crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
            construct: "TRUNCATE ... CONTINUE IDENTITY",
            reason: "SQLite keeps no sequence counter for a rowid alias, so the identifiers \
                     restart rather than continuing. The rows are still deleted.",
        });
    }

    let mut statements = Vec::with_capacity(truncate.table_names.len());
    for target in &truncate.table_names {
        // PostgreSQL does not apply policies to TRUNCATE: it needs the TRUNCATE
        // privilege and then empties the table. An RLS table is translated as a
        // view over a suffixed backing table, and the view's INSTEAD OF DELETE
        // trigger carries the policy predicate, so deleting through it would empty
        // only the admitted rows. Naming the backing table directly reproduces
        // PostgreSQL. Nothing observes it: only INSERT and UPDATE monitoring
        // triggers are generated, so a delete raises no validation event.
        let rls_backed = table_has_implicit_public_rls(schema, &target.name)?;
        let name = if rls_backed {
            append_suffix(&target.name, options.get_rls_table_suffix())
        } else {
            target.name.clone()
        };

        // ONLY and the trailing asterisk both concern table inheritance, which
        // CREATE TABLE ... INHERITS rejects outright, so no descendants can exist
        // and either spelling names just this table.
        let delete = Delete {
            tables: Vec::new(),
            from: FromTable::WithFromKeyword(vec![TableWithJoins {
                relation: TableFactor::Table {
                    name,
                    alias: None,
                    args: None,
                    with_hints: vec![],
                    version: None,
                    partitions: vec![],
                    json_path: None,
                    sample: None,
                    index_hints: vec![],
                    with_ordinality: false,
                },
                joins: vec![],
            }]),
            using: None,
            selection: None,
            returning: None,
            output: None,
            order_by: Vec::new(),
            limit: None,
            delete_token: AttachedToken::empty(),
            optimizer_hints: Vec::new(),
        };

        if rls_backed {
            // The backing name is deliberate and already final. Routing it through
            // the DELETE translator would resolve it against the logical schema,
            // which does not know the suffixed table, and could reapply the RLS
            // rewrite this branch exists to avoid. A TRUNCATE carries no predicate,
            // so there is nothing else for that pass to translate.
            statements.push(Statement::Delete(delete));
        } else {
            statements.push(delete.translate(schema, options)?);
        }
    }

    Ok(statements)
}

/// Rejects the `TRUNCATE` options with no SQLite form, per the reporting policy
/// in D2. Each would change which rows survive, so none may be dropped quietly.
fn reject_untranslatable_truncate_options(truncate: &Truncate) -> Result<(), Error> {
    let names = truncate
        .table_names
        .iter()
        .map(|target| target.name.to_string())
        .collect::<Vec<_>>()
        .join(", ");

    let unsupported = if matches!(truncate.cascade, Some(CascadeOption::Cascade)) {
        Some((
            "CASCADE",
            "it also empties every table holding a foreign key reference to the target, which \
             SQLite cannot express in one statement. Truncate those tables explicitly.",
        ))
    } else if truncate.partitions.is_some() {
        Some(("PARTITION", "SQLite has no table partitioning."))
    } else if truncate.on_cluster.is_some() {
        Some(("ON CLUSTER", "SQLite is not a clustered database."))
    } else if truncate.if_exists {
        Some((
            "IF EXISTS",
            "SQLite's DELETE has no such guard, so a missing table would raise an error rather \
             than being skipped.",
        ))
    } else {
        None
    };

    match unsupported {
        Some((option, reason)) => {
            Err(Error::UnsupportedSQLiteFeature(format!(
                "TRUNCATE {names} ... {option} cannot be translated to SQLite because {reason}"
            )))
        }
        None => Ok(()),
    }
}

/// Rejects `COPY`, which SQLite has no statement for in any of its forms.
///
/// Refusing rather than dropping matters most for `COPY ... FROM stdin`, whose
/// rows are carried inline in the migration file, so a silent drop loses data.
///
/// Translating that form into multi-row `INSERT`s would be the better answer
/// for a migration translator, and it is blocked upstream rather than merely
/// unimplemented. `Parser::parse_tab_value` flattens the payload into a
/// `Vec<Option<String>>` holding no row boundaries, since a tab and a newline
/// both just push a value, and a `\N` null leaves a phantom empty field that
/// desynchronises the list. Row structure cannot be recovered from the AST, and
/// the raw text is consumed by then. Pinned by
/// `sqlparser_still_flattens_copy_payload_rows` so this is revisited if fixed.
fn reject_copy(source: &CopySource, to: bool, target: &CopyTarget) -> Error {
    let subject = match source {
        CopySource::Table { table_name, .. } => format!("COPY {table_name}"),
        CopySource::Query(_) => "COPY (query)".to_owned(),
    };

    let advice = if to {
        "SQLite has no COPY statement, and the translator cannot write the destination. Export the \
         rows with your client instead."
    } else if matches!(target, CopyTarget::Stdin) {
        "SQLite has no COPY statement. The rows are carried inline in this statement, so they \
         would be lost outright. Rewrite the payload as INSERT statements."
    } else {
        "SQLite has no COPY statement, and the translator cannot read the source. Load the rows \
         with INSERT statements instead."
    };

    Error::UnsupportedSQLiteFeature(format!("{subject} cannot be translated. {advice}"))
}

/// Rejects `MERGE`, which has no SQLite form.
///
/// `INSERT ... ON CONFLICT DO UPDATE` resembles a translation without being
/// one. A `MERGE` `ON` clause is an ordinary join predicate, whereas an
/// upsert's conflict target must name a PRIMARY KEY or UNIQUE constraint, so
/// the common case of merging on a non-unique column has no upsert form at all.
/// Even when the columns are unique the two disagree on repeated matches: given
/// two source rows for one target row, PostgreSQL raises "MERGE command cannot
/// affect row a second time" and changes nothing, while an upsert applies both
/// and keeps the last, so a translation would silently produce data PostgreSQL
/// refuses. `WHEN NOT MATCHED BY SOURCE THEN DELETE` has no insert-shaped
/// equivalent whatsoever.
///
/// Recognising the narrow translatable subset would need the target's index
/// set, which the translation schema filters out, so the check is not even
/// possible today.
fn reject_merge(merge: &Merge) -> Error {
    Error::UnsupportedSQLiteFeature(format!(
        "MERGE INTO {} cannot be translated. SQLite has no MERGE statement, and \
         INSERT ... ON CONFLICT DO UPDATE is not equivalent: its conflict target must be a \
         PRIMARY KEY or UNIQUE constraint rather than an arbitrary join condition, and it applies \
         repeated matches in sequence where PostgreSQL refuses them. Write the INSERT, UPDATE, and \
         DELETE statements you mean, using INSERT ... ON CONFLICT DO UPDATE when the merge key is \
         genuinely unique.",
        merge.table
    ))
}

/// Rejects PostgreSQL's server-side prepared statements, which SQLite has no
/// statement form for.
///
/// In SQLite, preparing is a C API operation (`sqlite3_prepare_v2`) with no SQL
/// spelling and no server-side name to refer to afterwards, so none of these
/// has anything to emit. `EXECUTE` is the important one: it performs the work,
/// so dropping it loses whatever the migration intended, silently.
///
/// `DEALLOCATE` is refused as well, and it is the case worth justifying, since
/// its own effect is only to free a named plan and is therefore result-neutral
/// in isolation. It stays a hard error because a script that deallocates must
/// have prepared first, that `PREPARE` is an error, and a `DEALLOCATE` naming a
/// statement that cannot exist in the output is not something to accept
/// quietly.
fn reject_prepared_statement(keyword: &str, name: Option<&str>) -> Error {
    let subject = match name {
        Some(name) => format!("{keyword} {name}"),
        None => keyword.to_owned(),
    };

    Error::UnsupportedSQLiteFeature(format!(
        "{subject} cannot be translated. SQLite has no server-side prepared statements: preparing \
         is a C API call rather than a SQL statement, so there is no name to prepare, execute, or \
         deallocate. Inline the statement body at each use site, and let your SQLite driver \
         prepare it."
    ))
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
        let mut translated: Vec<Statement> = match self {
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
            Statement::AlterTable(alter_table) => {
                translate_alter_table(alter_table, schema, options)?
            }
            Statement::Truncate(truncate) => translate_truncate(truncate, schema, options)?,
            Statement::Copy { source, to, target, .. } => {
                return Err(reject_copy(source, *to, target));
            }
            Statement::Merge(merge) => return Err(reject_merge(merge)),
            Statement::Prepare { name, .. } => {
                return Err(reject_prepared_statement("PREPARE", Some(name.to_string().as_str())));
            }
            Statement::Execute { name, .. } => {
                return Err(reject_prepared_statement(
                    "EXECUTE",
                    name.as_ref().map(ToString::to_string).as_deref(),
                ));
            }
            Statement::Deallocate { name, prepare: _ } => {
                return Err(reject_prepared_statement(
                    "DEALLOCATE",
                    Some(name.to_string().as_str()),
                ));
            }
            unsupported_statement_patterns!() => Vec::new(),
        };

        // PostgreSQL numbered parameters (`$N`) become SQLite `?N` placeholders,
        // preserving the number so the bind index survives a round trip. Only
        // DML carries placeholders, so DDL output skips the walk.
        for statement in &mut translated {
            if matches!(
                statement,
                Statement::Query(_)
                    | Statement::Insert(_)
                    | Statement::Update(_)
                    | Statement::Delete(_)
            ) {
                rewrite_placeholders_for_sqlite(statement);
            }
        }

        Ok(translated)
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
