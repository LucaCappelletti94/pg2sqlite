//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `Statement` type.
//!
//! # How an untranslatable construct is reported
//!
//! Every statement leaves this module having produced one of four outcomes,
//! and there is no fifth. A silently empty result is a defect.
//!
//! 1. **Emitted.** One or more SQLite statements.
//! 2. **Hard error.** `Err(Error::TranslationRefusal)`. This is the DEFAULT.
//!    Use it when the construct carries meaning that cannot be preserved, when
//!    dropping it could change results, and for anything unrecognised.
//! 3. **Warn and drop.** `drop_with_warning`, permitted ONLY when the drop
//!    provably cannot affect a query result. If that cannot be stated in one
//!    sentence, it is not this outcome.
//! 4. **Consumed by the translation schema.** The statement is realised
//!    elsewhere in the pipeline, so contributing no SQLite statement is the
//!    COMPLETE translation rather than a loss. `CREATE POLICY` becoming a row
//!    level security view and trigger set is the clearest case. This list is
//!    closed and each member says where its effect is realised.
//!
//! Silence is additionally permitted where a caller-set option asks for it,
//! currently only `RoleTableAccess::Deny`, which is documented at the option
//! that sets it.
//!
//! The match over `Statement` has no wildcard arm on purpose: a new
//! `sqlparser` variant fails to compile until it is classified here, which is
//! stronger than discovering it at run time.

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
use sqlparser::ast::{
    AlterTable, AlterTableOperation, AlterTableType, BeginTransactionKind, BinaryOperator,
    CascadeOption, ColumnDef, ColumnOption, CopySource, CopyTarget, CreateFunction,
    CreateTableOptions, CreateView, Delete, DescribeAlias, DiscardObject, ExceptionWhen, Expr,
    FromTable, Ident, Merge, ObjectName, ObjectNamePart, ObjectType, Query, RenameTable,
    RenameTableNameKind, Set, SqlOption, Statement, TableFactor, TableWithJoins,
    TransactionAccessMode, TransactionMode, TransactionModifier, TriggerEvent, TriggerPeriod,
    Truncate, TruncateIdentityOption, UnaryOperator, VacuumStatement, ViewColumnDef,
    helpers::attached_token::AttachedToken,
};

use crate::{
    errors::Error,
    impls::{
        ast_builder,
        object_name::{
            append_suffix, last_ident, last_ident_value_or_display,
            normalize_schema_qualified_object_name_for_sqlite, quoted_ident,
            resolve_translation_table, sqlite_unqualified_object_name, translation_table_has_rls,
        },
        placeholder::rewrite_placeholders_for_sqlite,
        translator_impls::{
            column::translate_column_def,
            condition_injection::inject_condition_into_dml_statement,
            rls::{
                generate_readonly_rls_statements_with_context,
                generate_rls_statements_with_context, rename_table_for_rls,
                validate_table_policies, write_guard_condition_expr,
            },
            vector::{generate_vec0_statements, has_vector_columns},
        },
    },
    prelude::Pg2SqliteOptions,
    traits::translator::TranslatorWithContext,
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
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<(), Error> {
    for stmt in branch_statements {
        let mut translated_stmts = stmt.translate_with_warnings(schema, options, emit)?;
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
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<(), Error> {
    for (maybe_drop_trigger, translated_trigger) in
        create_trigger.translate_with_warnings(schema, options, emit)?
    {
        if let Some(drop_trigger) = maybe_drop_trigger {
            statements.push(drop_trigger.into());
        }
        statements.push(translated_trigger.into());
    }
    Ok(())
}

/// Statements that move data into or out of the database. SQLite has no
/// statement form for any of them, and dropping one loses the rows it carries.
macro_rules! data_movement_patterns {
    () => {
        Statement::Directory { .. }
            | Statement::ExportData(_)
            | Statement::Unload { .. }
            | Statement::LoadData { .. }
            | Statement::CopyIntoSnowflake { .. }
            | Statement::Put { .. }
            | Statement::Remove { .. }
            | Statement::List { .. }
    };
}

/// Procedural control flow and diagnostics. Each carries the script's own
/// logic, so dropping one changes what the script does.
macro_rules! control_flow_patterns {
    () => {
        Statement::Case(_)
            | Statement::While { .. }
            | Statement::Raise { .. }
            | Statement::RaisError { .. }
            | Statement::Throw { .. }
            | Statement::Print { .. }
            | Statement::Return { .. }
            | Statement::Assert { .. }
    };
}

/// Cursor statements. A cursor loop reads and often writes rows, so the four
/// together perform the work the script intended.
macro_rules! cursor_patterns {
    () => {
        Statement::Declare { .. }
            | Statement::Open { .. }
            | Statement::Fetch { .. }
            | Statement::Close { .. }
    };
}

/// Stored procedures and macros. Their bodies are statements that would be
/// lost, and `CALL` performs the work.
macro_rules! procedural_patterns {
    () => {
        Statement::Call(_)
            | Statement::CreateProcedure { .. }
            | Statement::DropProcedure { .. }
            | Statement::CreateMacro { .. }
    };
}

/// Session state other than `SET`, which needs per-setting treatment. Each of
/// these changes how later statements resolve names or behave.
macro_rules! session_state_patterns {
    () => {
        Statement::Reset(_) | Statement::Use { .. } | Statement::AlterSession { .. }
    };
}

/// PostgreSQL extensibility objects. This group is a hard error rather than a
/// warned drop because its absence is NOT reported at the point of use:
/// `expr.rs` passes an unrecognised `BinaryOperator::Custom` straight through,
/// so dropping the definition would leave the emitted SQL to fail at run time.
macro_rules! extensibility_patterns {
    () => {
        Statement::CreateOperator(_)
            | Statement::CreateOperatorClass(_)
            | Statement::CreateOperatorFamily(_)
            | Statement::AlterOperator(_)
            | Statement::AlterOperatorClass(_)
            | Statement::AlterOperatorFamily(_)
            | Statement::DropOperator { .. }
            | Statement::DropOperatorClass { .. }
            | Statement::DropOperatorFamily { .. }
            | Statement::CreateTextSearch(_)
            | Statement::AlterTextSearch(_)
    };
}

/// Renames of an existing object. SQLite can only drop and recreate, which
/// needs the object's current definition, and the translation schema does not
/// carry index definitions, so this cannot be rewritten here.
///
/// `ALTER VIEW ... AS` is deliberately not one of these. It is not a rename:
/// it carries the new definition with it, so nothing has to be looked up, and
/// it takes the same drop-then-create path as `CREATE OR REPLACE VIEW`.
macro_rules! alter_in_place_patterns {
    () => {
        Statement::AlterIndex { .. }
    };
}

/// Statements that address a database rather than objects inside one.
macro_rules! database_level_patterns {
    () => {
        Statement::CreateDatabase { .. }
            | Statement::AttachDuckDBDatabase { .. }
            | Statement::DetachDuckDBDatabase { .. }
    };
}

/// Reason shared by every statement that only reports server state.
const REASON_INTROSPECTION: &str =
    "SQLite has no catalog or settings statement, and reporting state cannot affect a result.";

/// Reason shared by role, privilege, and ownership statements.
const REASON_ACCESS_CONTROL: &str =
    "SQLite has no role or privilege model, so there is nothing for the statement to act on.";

/// Reason shared by statements that only administer the server.
const REASON_ADMINISTRATION: &str =
    "SQLite has no server to administer, and the request cannot affect a result.";

/// Reason shared by planner and locking hints.
const REASON_HINT: &str = "SQLite plans each statement itself and locks the whole database per transaction, so the hint \
     has no counterpart and no effect on results.";

/// Reason shared by type-like object definitions the host registers instead.
const REASON_HOST_REGISTERED: &str = "SQLite has no SQL form for this object. Where one is needed at run time it is registered \
     through SQLite's C API by the host application, so the definition cannot be emitted.";

/// Drops a statement with a `LossyDrop` warning. Outcome 3 of the reporting
/// policy in this module's documentation, so `construct` names the statement
/// and `reason` states why the drop cannot change a result.
fn drop_with_warning(
    construct: &'static str,
    reason: &'static str,
    emit: crate::warnings::WarningSink<'_>,
) -> Vec<Statement> {
    emit(crate::warnings::TranslationWarning::LossyDrop {
        construct: construct.to_string(),
        reason: reason.to_string(),
    });
    Vec::new()
}

/// Refuses `statement`, outcome 2 of the reporting policy. Renders the
/// statement so the message names what was refused rather than only its kind.
fn reject_unsupported_statement(statement: &Statement, reason: &str) -> Error {
    Error::forward_refusal(format!("{statement} has no SQLite equivalent. {reason}"))
}

/// Emits a view definition, preceded by `DROP VIEW IF EXISTS` when it replaces
/// one, since SQLite has no `CREATE OR REPLACE VIEW`.
///
/// Shared with the `ALTER VIEW ... AS` arm, which is the same redefinition
/// under another spelling.
fn translate_view_definition(
    create_view: &CreateView,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let mut statements: Vec<Statement> = Vec::new();
    if create_view.or_replace {
        statements.push(Statement::Drop {
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
    statements.push(create_view.translate_with_warnings(schema, options, emit)?.into());
    Ok(statements)
}

/// Reads `ALTER VIEW ... AS` as the replacement it is.
///
/// `with_options` travels rather than being dropped, so the `CREATE VIEW`
/// translator refuses an option it cannot express instead of this arm quietly
/// losing it.
fn alter_view_as_create_view(
    name: &ObjectName,
    columns: &[Ident],
    query: &Query,
    with_options: &[SqlOption],
) -> CreateView {
    CreateView {
        or_alter: false,
        or_replace: true,
        materialized: false,
        secure: false,
        copy_grants: false,
        name: name.clone(),
        name_before_not_exists: false,
        columns: columns
            .iter()
            .map(|name| ViewColumnDef { name: name.clone(), data_type: None, options: None })
            .collect(),
        query: Box::new(query.clone()),
        options: if with_options.is_empty() {
            CreateTableOptions::None
        } else {
            CreateTableOptions::With(with_options.to_vec())
        },
        cluster_by: Vec::new(),
        comment: None,
        if_not_exists: false,
        temporary: false,
        to: None,
        params: None,
        with_no_schema_binding: false,
    }
}

/// Reason shared by the publish and subscribe statements.
const REASON_PUB_SUB: &str = "SQLite has no channel to publish on or listen to.";

/// Reason shared by type and domain definitions: a translated column carries
/// the underlying SQLite storage class, so the named wrapper has no use.
const REASON_TYPE_DEFINITION: &str = "SQLite has no composite, enum, or domain types, and a column of one is translated to the \
     storage class underneath it.";

/// Reason shared by foreign data and credential definitions.
const REASON_FOREIGN_DATA: &str = "SQLite reads only its own database file, so it has no foreign data layer for the definition \
     to configure.";

/// Reason a function definition is dropped. A trigger function is not lost: its
/// body is inlined into every trigger that calls it, which is what
/// `create_trigger.rs` uses `function_body_with_context` for. Any other
/// function has to be registered by the host, the same way this crate expects
/// `current_setting` to be registered for row level security.
const REASON_FUNCTION: &str = "SQLite has no statement that defines a function. A trigger function's body is inlined into \
     the triggers that call it, and any other function must be registered by the host through \
     SQLite's C API.";

/// Reason an extension declaration is dropped: extension functions are
/// translated by name, independently of any declaration.
const REASON_EXTENSION: &str = "SQLite has no extension registry, and the functions an extension provides are translated by \
     name whether or not it is declared.";

/// Reason a sequence definition is dropped. This one is only result-neutral
/// because every function that reads a sequence (`nextval`, `currval`,
/// `lastval`, `setval`) is already refused in `function.rs`, so a translation
/// that uses the sequence cannot succeed quietly.
const REASON_SEQUENCE: &str = "SQLite has no sequences, and every function that reads one is refused, so nothing can use \
     this definition without being reported.";

/// Reason a `SET` of a result-neutral setting is dropped.
const REASON_SET_NEUTRAL: &str = "The setting governs how long a statement may run, how much it reports, or how a literal was \
     parsed, so SQLite needs no counterpart and the result cannot change.";

/// Reason the PostgreSQL transaction characteristics are dropped.
const REASON_TRANSACTION_CHARACTERISTICS: &str = "SQLite has one isolation level, and it serialises writers, so it is at least as strict as any \
     level PostgreSQL can name and the transaction sees the same rows either way.";

/// Reason the `VACUUM` options and table name are dropped.
const REASON_VACUUM_OPTIONS: &str = "SQLite's VACUUM rebuilds the whole database and takes no options, so the request is carried \
     out on a larger scope than asked for rather than lost.";

/// Reason a wait statement is dropped.
const REASON_WAIT: &str = "SQLite has no statement that waits, and waiting changes when a result arrives rather than \
     what it is.";

/// Reason a `DROP` of a PostgreSQL-only object kind is a warned drop: the
/// object never reached the SQLite output, because the statement that would
/// have created it is itself dropped or refused, so there is nothing to remove.
const REASON_DROP_OF_ABSENT_OBJECT: &str = "SQLite has no object of this kind, and the matching CREATE never emitted one, so the drop \
     removes nothing that exists.";

/// Static label for the warning emitted when a `DROP` names an object kind
/// SQLite does not have.
fn drop_label(object_type: ObjectType) -> &'static str {
    match object_type {
        ObjectType::Schema => "DROP SCHEMA",
        ObjectType::Role => "DROP ROLE",
        ObjectType::User => "DROP USER",
        ObjectType::Sequence => "DROP SEQUENCE",
        ObjectType::Type => "DROP TYPE",
        ObjectType::Collation => "DROP COLLATION",
        ObjectType::Stage => "DROP STAGE",
        ObjectType::Stream => "DROP STREAM",
        ObjectType::Warehouse => "DROP WAREHOUSE",
        ObjectType::Table
        | ObjectType::View
        | ObjectType::Index
        | ObjectType::MaterializedView
        | ObjectType::Database => "DROP",
    }
}

fn translate_create_table(
    create_table: &sqlparser::ast::CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    if let Some(role_filtered) =
        translate_create_table_for_role(create_table, schema, options, emit)?
    {
        return Ok(role_filtered);
    }

    // The RLS pipeline gets the schema's own node, not this statement's.
    // After an ALTER TABLE ADD or DROP COLUMN the schema holds a modified
    // clone, and sql-traits' `policies` answers empty for a node the graph no
    // longer matches while `has_row_level_security` still answers true, so the
    // raw node degrades the wrapper to deny-by-default. The asymmetry is
    // written up in docs/sql_traits_policies_on_stale_node.md. The role arm
    // below already resolves the same way.
    let table = schema
        .table(create_table.table_schema(), create_table.table_name())
        .unwrap_or(create_table);
    if table.has_row_level_security(schema)? {
        validate_table_policies(table, schema, options)?;
        let rls_statements = generate_rls_statements_with_context(table, schema, options, emit)?;
        return build_create_table_statements(
            create_table,
            schema,
            options,
            Some(rls_statements),
            emit,
        );
    }

    build_create_table_statements(create_table, schema, options, None, emit)
}

fn translate_create_table_for_role(
    create_table: &sqlparser::ast::CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<Vec<Statement>>, Error> {
    let Some(role) = resolve_session_role(schema, options) else {
        return Ok(None);
    };
    let Some(table) = schema.table(create_table.table_schema(), create_table.table_name()) else {
        return Ok(None);
    };

    if !table.can_select(role, schema)? {
        return Ok(Some(Vec::new()));
    }

    let is_readonly = !table.can_write(role, schema)?;
    if table.has_row_level_security(schema)? {
        validate_table_policies(table, schema, options)?;
        let rls_statements = if is_readonly {
            generate_readonly_rls_statements_with_context(table, schema, options, emit)?
        } else {
            generate_rls_statements_with_context(table, schema, options, emit)?
        };
        let statements = build_create_table_statements(
            create_table,
            schema,
            options,
            Some(rls_statements),
            emit,
        )?;
        return Ok(Some(statements));
    }

    if is_readonly {
        let mut statements =
            build_create_table_statements(create_table, schema, options, None, emit)?;
        append_readonly_deny_triggers(&mut statements, options)?;
        return Ok(Some(statements));
    }

    Ok(None)
}

fn build_create_table_statements(
    create_table: &sqlparser::ast::CreateTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    rls_statements: Option<Vec<Statement>>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    let mut statements = if let Some(rls_statements) = rls_statements {
        let translated_table = create_table.translate_with_warnings(schema, options, emit)?;
        let inner_table = rename_table_for_rls(&translated_table, options, schema);
        let mut statements = vec![Statement::CreateTable(inner_table)];
        statements.extend(rls_statements);
        statements
    } else {
        vec![Statement::CreateTable(create_table.translate_with_warnings(schema, options, emit)?)]
    };

    append_vec0_statements_if_needed(&mut statements, create_table, schema, options, emit)?;
    Ok(statements)
}

const READONLY_DENY_TRIGGER_VERBS: [&str; 3] = ["insert", "update", "delete"];

/// Appends deny triggers so ordinary writes to a read-only non-RLS table fail.
fn append_readonly_deny_triggers(
    statements: &mut Vec<Statement>,
    options: &crate::options::TranslationContext<'_>,
) -> Result<(), Error> {
    let Some(Statement::CreateTable(create_table)) = statements.first() else {
        return Ok(());
    };
    let sqlite_name = sqlite_unqualified_object_name(&create_table.name);
    let table_ident = last_ident(&sqlite_name)
        .map_or_else(|| sqlite_name.to_string(), |ident| ident.value.clone());

    let marker = options.get_readonly_deny_trigger_suffix();
    let name = |value: &str| ObjectName(vec![ObjectNamePart::Identifier(quoted_ident(value))]);
    let message = format!("permission denied: {table_ident} is read-only for this role");
    let condition = write_guard_condition_expr(None, options);

    let mut triggers = Vec::with_capacity(READONLY_DENY_TRIGGER_VERBS.len());
    for verb in READONLY_DENY_TRIGGER_VERBS {
        let trigger_name = format!("{table_ident}{marker}_{verb}");
        reject_reserved_name_collision(options, &table_ident, &trigger_name)?;
        let event = match verb {
            "insert" => TriggerEvent::Insert,
            "update" => TriggerEvent::Update(Vec::new()),
            "delete" => TriggerEvent::Delete,
            _ => unreachable!("the read-only trigger verb list is closed"),
        };
        triggers.push(ast_builder::trigger(
            name(&trigger_name),
            name(&table_ident),
            TriggerPeriod::Before,
            event,
            false,
            condition.clone(),
            vec![ast_builder::raise_statement("ABORT", Some(&message), None)],
        ));
    }

    statements.extend(triggers);
    Ok(())
}

/// Errors when the translation unit already declares a table, index, trigger,
/// or view whose SQLite-unqualified name collides with a reserved deny-trigger
/// name. The declared-name catalog comes from `populate_prewalk_catalogs`,
/// since the translation schema omits index and trigger definitions.
fn reject_reserved_name_collision(
    options: &crate::options::TranslationContext<'_>,
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
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<(), Error> {
    if has_vector_columns(create_table) {
        statements.extend(generate_vec0_statements(create_table, schema, options, emit)?);
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
    options: &crate::options::TranslationContext<'_>,
) -> Result<RoleTableAccess, Error> {
    let Some(role) = resolve_session_role(schema, options) else {
        return Ok(RoleTableAccess::Allow);
    };

    let Some(table) = resolve_translation_table(schema, table_name)? else {
        return Err(Error::TableNotFoundInSchema { table_name: table_name.to_string() });
    };

    if table.can_select(role, schema)? {
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
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    // An RLS table is translated as a view over a suffixed backing table, and
    // a view cannot be altered, so the statement lands on the backing table
    // the way TRUNCATE's does. No wrapper rebuild is needed: every generated
    // RLS object is built from the one final schema snapshot, so the view and
    // triggers already speak the post-ALTER shape, and this redirected ALTER
    // is what brings the backing table up to it. A rename never reaches here
    // on a secured table, `reject_rename_of_secured_table` refuses it first.
    let normalized_name = if translation_table_has_rls(schema, &alter_table.name)? {
        append_suffix(
            &normalize_schema_qualified_object_name_for_sqlite(schema, &alter_table.name)?,
            options.get_rls_table_suffix(),
        )
    } else {
        normalize_schema_qualified_object_name_for_sqlite(schema, &alter_table.name)?
    };
    reject_untranslatable_alter_table_clauses(alter_table, schema)?;

    let mut statements = Vec::with_capacity(alter_table.operations.len());
    for operation in &alter_table.operations {
        let Some(translated) =
            translate_alter_table_operation(operation, alter_table, schema, options, emit)?
        else {
            continue;
        };
        // Every field is named rather than spread from the input, so a field
        // added upstream fails to compile here instead of reaching SQLite
        // unexamined. Four of them did exactly that before this was tightened.
        statements.push(Statement::AlterTable(AlterTable {
            name: normalized_name.clone(),
            operations: vec![translated],
            // Cleared above: refused when it cannot be proven redundant.
            if_exists: false,
            // Inheritance, which `CREATE TABLE ... INHERITS` already refuses,
            // so this names the same single table either way.
            only: false,
            location: None,
            on_cluster: None,
            table_type: None,
            end_token: alter_table.end_token.clone(),
        }));
    }

    Ok(statements)
}

/// Refuses the `ALTER TABLE` clauses that cannot reach SQLite, so
/// [`translate_alter_table`] can clear the rest when it rebuilds the statement.
///
/// SQLite has none of these, but that is not the interesting part: nearly
/// everything this crate translates is a syntax error in SQLite. What decides
/// each answer is whether the clause carries meaning that would be lost.
///
/// `ON CLUSTER` is ClickHouse and the three `table_type` spellings are
/// Snowflake. PostgreSQL rejects all of them outright, so a file carrying one
/// is not the input this crate accepts, which is the reason worth reporting.
/// Only `ICEBERG` currently reaches this: `DYNAMIC` and `EXTERNAL` are refused
/// by the parser under the PostgreSQL dialect, as is Hive's `SET LOCATION`, and
/// all three are named anyway so a parser change cannot reopen the hole
/// quietly.
///
/// The existence check runs for every operation, guarded or not. A rename is
/// the one operation that needs nothing from the schema, so it was the one
/// that could reach SQLite naming a table the batch never declared, failing
/// on apply as `no such table`. The refusal is one message for both forms:
/// for the guarded form emitting nothing would reproduce PostgreSQL, which
/// skips a missing table where SQLite raises, but the schema is built from
/// the input rather than from a live database, so an absent table almost
/// always means the CREATE TABLE was left out of the batch, and that is
/// indistinguishable from a deliberate guard.
fn reject_untranslatable_alter_table_clauses(
    alter_table: &AlterTable,
    schema: &ParserDB,
) -> Result<(), Error> {
    let foreign = if alter_table.on_cluster.is_some() {
        Some("ON CLUSTER, which is ClickHouse")
    } else if let Some(table_type) = alter_table.table_type.as_ref() {
        Some(match table_type {
            AlterTableType::Iceberg => "ICEBERG, which is Snowflake",
            AlterTableType::Dynamic => "DYNAMIC, which is Snowflake",
            AlterTableType::External => "EXTERNAL, which is Snowflake",
        })
    } else if alter_table.location.is_some() {
        Some("SET LOCATION, which is Hive")
    } else {
        None
    };

    if let Some(clause) = foreign {
        return Err(Error::forward_refusal(format!(
            "ALTER TABLE {} carries {clause}. PostgreSQL rejects that spelling, so a file \
             containing it is not the input this crate translates. Remove the clause.",
            alter_table.name
        )));
    }

    if resolve_translation_table(schema, &alter_table.name)?.is_none() {
        return Err(Error::forward_refusal(format!(
            "ALTER TABLE {} names a table the translation schema does not declare. The schema \
             is built from the statements in the batch rather than from a live database, so an \
             absent table almost always means its CREATE TABLE was left out. Include the \
             table's definition in the same translation batch.",
            alter_table.name
        )));
    }

    Ok(())
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
///
/// A rename is checked rather than passed through. PostgreSQL spells it
/// `RENAME TO <bare name>` and nothing else, so the two spellings `sqlparser`
/// also accepts are refused: `RENAME AS` is MySQL, and a schema-qualified
/// target is a syntax error in PostgreSQL. Both are also syntax errors in
/// SQLite (`near "AS"` and `near "."`), so passing either through would emit
/// SQL that cannot run.
fn translate_alter_table_operation(
    operation: &AlterTableOperation,
    alter_table: &AlterTable,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<AlterTableOperation>, Error> {
    match operation {
        AlterTableOperation::RenameTable { table_name } => {
            reject_untranslatable_rename_target(table_name, alter_table)?;
            Ok(Some(operation.clone()))
        }
        AlterTableOperation::RenameColumn { .. } | AlterTableOperation::DropColumn { .. } => {
            Ok(Some(operation.clone()))
        }
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
                // SQLite requires of a non-literal DEFAULT all apply. No primary
                // key columns are passed: SQLite cannot add a primary key with
                // ALTER TABLE, so an added column is never its rowid alias.
                column_def: translate_column_def(
                    column_def,
                    &alter_table.name,
                    &[],
                    schema,
                    options,
                    emit,
                )?,
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
            Err(Error::forward_refusal(format!(
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
        return Err(Error::forward_refusal(format!(
            "ALTER TABLE {table_name} ADD COLUMN {} cannot carry a {constraint} constraint: \
             SQLite rejects it because enforcing the constraint would require rewriting the \
             table. Declare the column without it, then add a separate unique index.",
            column_def.name
        )));
    }
    Ok(())
}

/// Refuses the rename spellings PostgreSQL does not have.
///
/// PostgreSQL accepts exactly `ALTER TABLE <name> RENAME TO <bare name>`.
/// `sqlparser`'s PostgreSQL dialect is looser and also produces
/// `RenameTableNameKind::As` for `RENAME AS`, which is MySQL, and accepts a
/// schema-qualified target. Both were verified rejected by PostgreSQL 16, so
/// input containing either is not the PostgreSQL this crate translates, and
/// both are syntax errors in SQLite as well.
fn reject_untranslatable_rename_target(
    table_name: &RenameTableNameKind,
    alter_table: &AlterTable,
) -> Result<(), Error> {
    let RenameTableNameKind::To(target) = table_name else {
        return Err(Error::forward_refusal(format!(
            "ALTER TABLE {} RENAME AS is MySQL syntax, which PostgreSQL rejects and SQLite rejects \
             with `near \"AS\": syntax error`. Write RENAME TO instead.",
            alter_table.name
        )));
    };

    if target.0.len() > 1 {
        return Err(Error::forward_refusal(format!(
            "ALTER TABLE {} RENAME TO {target} cannot name a schema for the new table. PostgreSQL \
             rejects a qualified target, since a rename never moves a table between schemas, and \
             SQLite rejects it too. Name the new table alone.",
            alter_table.name
        )));
    }

    Ok(())
}

/// Refuses `RENAME TABLE`, which is MySQL rather than PostgreSQL.
///
/// `sqlparser` parses it under the PostgreSQL dialect, but PostgreSQL 16
/// rejects it outright, so a file containing it is not the input this crate
/// accepts. SQLite has no `RENAME` statement either, only the `ALTER TABLE`
/// clause, so passing it through would emit `near "RENAME": syntax error`.
fn reject_rename_table(renames: &[RenameTable]) -> Error {
    let rewritten = renames
        .iter()
        .map(|rename| format!("ALTER TABLE {} RENAME TO {}", rename.old_name, rename.new_name))
        .collect::<Vec<_>>()
        .join("; ");

    Error::forward_refusal(format!(
        "RENAME TABLE is MySQL syntax, which PostgreSQL rejects, and SQLite has no RENAME \
         statement at all. Write it as PostgreSQL would: {rewritten}."
    ))
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
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    reject_untranslatable_truncate_options(truncate)?;

    if matches!(truncate.identity, Some(TruncateIdentityOption::Continue)) {
        emit(crate::warnings::TranslationWarning::LossyDrop {
            construct: "TRUNCATE ... CONTINUE IDENTITY".to_string(),
            reason: "SQLite keeps no sequence counter for a rowid alias, so the identifiers \
                 restart rather than continuing. The rows are still deleted."
                .to_string(),
        });
    }

    let mut statements = Vec::with_capacity(truncate.table_names.len());
    for target in &truncate.table_names {
        // PostgreSQL does not apply policies to TRUNCATE: it needs the TRUNCATE
        // privilege and then empties the table. An RLS table is translated as a
        // view over a suffixed backing table, and the view's INSTEAD OF DELETE
        // trigger carries the policy predicate, so deleting through it would
        // empty only the admitted rows. Naming the backing table
        // directly reproduces PostgreSQL. Nothing observes it: only
        // INSERT and UPDATE monitoring triggers are generated, so a
        // delete raises no validation event.
        let rls_backed = translation_table_has_rls(schema, &target.name)?;
        let name = if rls_backed {
            append_suffix(&target.name, options.get_rls_table_suffix())
        } else {
            target.name.clone()
        };

        // ONLY and the trailing asterisk both concern table inheritance, which
        // CREATE TABLE ... INHERITS rejects outright, so no descendants can
        // exist and either spelling names just this table.
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
            // The backing name is deliberate and already final. Routing it
            // through the DELETE translator would resolve it
            // against the logical schema, which does not know the
            // suffixed table, and could reapply the RLS
            // rewrite this branch exists to avoid. A TRUNCATE carries no
            // predicate, so there is nothing else for that pass to
            // translate.
            statements.push(Statement::Delete(delete));
        } else {
            statements.push(delete.translate_with_warnings(schema, options, emit)?);
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
            Err(Error::forward_refusal(format!(
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

    Error::forward_refusal(format!("{subject} cannot be translated. {advice}"))
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
    Error::forward_refusal(format!(
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

    Error::forward_refusal(format!(
        "{subject} cannot be translated. SQLite has no server-side prepared statements: preparing \
         is a C API call rather than a SQL statement, so there is no name to prepare, execute, or \
         deallocate. Inline the statement body at each use site, and let your SQLite driver \
         prepare it."
    ))
}

/// Translates PostgreSQL `EXPLAIN` to SQLite `EXPLAIN QUERY PLAN`.
///
/// The inner statement is translated too. Without that the plan would be
/// computed over PostgreSQL SQL that SQLite cannot parse, so the emitted
/// statement would not run.
///
/// `EXPLAIN ANALYZE` is refused: PostgreSQL executes the statement and reports
/// real timings, whereas `EXPLAIN QUERY PLAN` never executes anything, so an
/// `EXPLAIN ANALYZE INSERT ...` would quietly stop writing.
fn translate_explain(
    analyze: bool,
    statement: &Statement,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    if analyze {
        return Err(Error::forward_refusal("EXPLAIN ANALYZE cannot be translated. PostgreSQL runs the statement and reports real \
                 timings, while SQLite's EXPLAIN QUERY PLAN only describes the plan and never executes, \
                 so any write the statement performs would be lost. Run the statement itself, or use a \
                 plain EXPLAIN for the plan."
            .to_owned()));
    }

    let mut translated = statement.translate_with_warnings(schema, options, emit)?;
    if translated.len() != 1 {
        return Err(Error::forward_refusal(format!(
            "EXPLAIN cannot be translated because its statement expands to {} SQLite statements, \
         and a plan can only describe one. Explain the individual statements instead.",
            translated.len()
        )));
    }

    Ok(vec![Statement::Explain {
        describe_alias: DescribeAlias::Explain,
        analyze: false,
        verbose: false,
        query_plan: true,
        estimate: false,
        statement: Box::new(translated.remove(0)),
        format: None,
        options: None,
    }])
}

/// Builds SQLite's `BEGIN`, carrying only a locking modifier SQLite has of its
/// own.
fn sqlite_begin(modifier: Option<TransactionModifier>) -> Statement {
    Statement::StartTransaction {
        modes: Vec::new(),
        begin: true,
        transaction: None,
        modifier,
        statements: Vec::new(),
        exception: None,
        has_end_keyword: false,
    }
}

/// Translates `START TRANSACTION` and `BEGIN` to SQLite's `BEGIN`.
///
/// `START TRANSACTION` is a syntax error in SQLite whatever follows it, so the
/// spelling itself has to change, and the PostgreSQL clauses go with it.
///
/// An isolation level is dropped with a warning because SQLite is at least as
/// strict as any level PostgreSQL can name: it serialises writers, so asking
/// for something weaker and getting something stronger cannot change which rows
/// a transaction sees. `READ WRITE` is the default and goes the same way.
///
/// `READ ONLY` is refused instead. PostgreSQL rejects a write inside such a
/// transaction, so dropping the clause would turn an error the author relies on
/// into a write that succeeds.
///
/// `DEFERRED`, `IMMEDIATE`, and `EXCLUSIVE` are SQLite's own locking modifiers
/// and are kept.
fn translate_start_transaction(
    statement: &Statement,
    modes: &[TransactionMode],
    transaction: Option<&BeginTransactionKind>,
    modifier: Option<TransactionModifier>,
    statements: &[Statement],
    exception: Option<&[ExceptionWhen]>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    if !statements.is_empty() || exception.is_some() {
        return Err(reject_unsupported_statement(
            statement,
            "SQLite has no BEGIN block outside a trigger body, so the statements it carries would \
             be lost.",
        ));
    }

    if modes.contains(&TransactionMode::AccessMode(TransactionAccessMode::ReadOnly)) {
        return Err(reject_unsupported_statement(
            statement,
            "SQLite has no read-only transaction, so a write PostgreSQL would reject inside one \
             would succeed here. Set the connection read-only with PRAGMA query_only = 1 instead.",
        ));
    }

    let modifier = match modifier {
        // MS-SQL block structure rather than a transaction characteristic.
        Some(TransactionModifier::Try | TransactionModifier::Catch) => {
            return Err(reject_unsupported_statement(
                statement,
                "TRY and CATCH blocks are MS-SQL, and SQLite has no error handling in SQL.",
            ));
        }
        kept => kept,
    };

    if !modes.is_empty() {
        drop_with_warning("BEGIN", REASON_TRANSACTION_CHARACTERISTICS, emit);
    }

    Ok(vec![Statement::StartTransaction {
        modes: Vec::new(),
        begin: true,
        // SQLite accepts `BEGIN TRANSACTION` but neither `WORK` nor `TRAN`.
        transaction: matches!(transaction, Some(BeginTransactionKind::Transaction))
            .then_some(BeginTransactionKind::Transaction),
        modifier,
        statements: Vec::new(),
        exception: None,
        has_end_keyword: false,
    }])
}

/// Translates `COMMIT`, turning `AND CHAIN` into the `BEGIN` it means.
///
/// `AND CHAIN` commits and immediately opens another transaction with the same
/// characteristics, so emitting `COMMIT` alone would leave the statements after
/// it outside any transaction and a later `ROLLBACK` with nothing to undo.
/// Emitting the `BEGIN` reproduces it, minus characteristics SQLite does not
/// have anyway.
fn translate_commit(
    statement: &Statement,
    chain: bool,
    end: bool,
    modifier: Option<TransactionModifier>,
) -> Result<Vec<Statement>, Error> {
    if let Some(TransactionModifier::Try | TransactionModifier::Catch) = modifier {
        return Err(reject_unsupported_statement(
            statement,
            "TRY and CATCH blocks are MS-SQL, and SQLite has no error handling in SQL.",
        ));
    }

    // `END` is SQLite's own synonym for `COMMIT`, so the spelling survives.
    let mut translated = vec![Statement::Commit { chain: false, end, modifier: None }];
    if chain {
        translated.push(sqlite_begin(None));
    }
    Ok(translated)
}

/// Translates `ROLLBACK`, turning `AND CHAIN` into the `BEGIN` it means, for
/// the same reason as [`translate_commit`]. `ROLLBACK TO SAVEPOINT` is SQLite's
/// own spelling and passes through.
fn translate_rollback(chain: bool, savepoint: Option<&Ident>) -> Vec<Statement> {
    let mut translated = vec![Statement::Rollback { chain: false, savepoint: savepoint.cloned() }];
    if chain {
        translated.push(sqlite_begin(None));
    }
    translated
}

/// Emits a bare `VACUUM`.
///
/// SQLite's `VACUUM` takes an optional SCHEMA name and nothing else, so a
/// PostgreSQL option list has no counterpart and a table name is actively
/// harmful: SQLite reads it as a schema and answers `unknown database t`.
/// Rebuilding one table is not expressible, and vacuuming the whole database
/// achieves what the statement asked for on a larger scope, so this is a warned
/// drop rather than an error.
fn translate_vacuum(
    vacuum: &VacuumStatement,
    emit: crate::warnings::WarningSink<'_>,
) -> Vec<Statement> {
    let bare = VacuumStatement {
        full: false,
        sort_only: false,
        delete_only: false,
        reindex: false,
        recluster: false,
        table_name: None,
        threshold: None,
        boost: false,
    };

    if *vacuum != bare {
        drop_with_warning("VACUUM", REASON_VACUUM_OPTIONS, emit);
    }

    vec![Statement::Vacuum(bare)]
}

/// PostgreSQL run-time configuration parameters whose value cannot change what
/// a translated statement does. A `SET` naming one is dropped with a warning,
/// every other `SET` is refused.
///
/// Built from PostgreSQL's own classification rather than by hand. `SET` can
/// change 187 parameters (`pg_settings` where `context` is `user` or
/// `superuser`, measured against PostgreSQL 16.14). Four of the 26 categories
/// carry query semantics: `Client Connection Defaults / Statement Behavior`,
/// `Client Connection Defaults / Locale and Formatting`, and both `Version and
/// Platform Compatibility` categories. The other 136 parameters govern
/// planning, memory, durability, logging, and statistics, so the rows a query
/// returns are identical either way, and they are all here.
///
/// `gin_fuzzy_search_limit` is the one the categories get wrong, and it is
/// excluded: it sits in `Other Defaults` yet is documented as a "soft upper
/// limit of the size of the set returned by GIN index scans", so setting it
/// drops rows.
///
/// Twelve more are included from the semantic categories, because each is
/// provably neutral and together they are the `pg_dump` preamble, which must
/// keep translating. The five timeouts bound how long a statement may run.
/// `client_min_messages` bounds what it says. `check_function_bodies` only
/// defers validation of a routine body. `client_encoding` names the wire
/// encoding, and SQLite stores text as UTF-8 regardless.
/// `default_table_access_method` names a storage engine with no SQLite
/// counterpart. `escape_string_warning` only emits a warning. `xmloption`
/// decides `DOCUMENT` against `CONTENT` when casting XML, which this crate has
/// no mapping for in the first place. `standard_conforming_strings` is the one
/// that needs its VALUE checked, in [`translate_set`].
///
/// Refused, by way of illustration of what those categories otherwise hold:
/// `search_path` decides which table a bare name means, `row_security` decides
/// whether policies apply, `session_replication_role` stops triggers and
/// therefore foreign key checks from firing, `TimeZone` and `DateStyle` decide
/// how a value reads, `array_nulls` decides whether `NULL` in an array literal
/// is a null or the string, and `transform_null_equals` rewrites `x = NULL`
/// into `x IS NULL`.
///
/// Sorted for [`is_result_neutral_setting`], which binary searches it, and
/// asserted sorted by a test. A parameter a later PostgreSQL adds is absent
/// until someone adds it here and is therefore refused, which is the intended
/// direction to fail in.
const RESULT_NEUTRAL_SETTINGS: [&str; 147] = [
    "allow_in_place_tablespaces",
    "allow_system_table_mods",
    "application_name",
    "backend_flush_after",
    "backtrace_functions",
    "check_function_bodies",
    "client_connection_check_interval",
    "client_encoding",
    "client_min_messages",
    "commit_delay",
    "commit_siblings",
    "compute_query_id",
    "constraint_exclusion",
    "cpu_index_tuple_cost",
    "cpu_operator_cost",
    "cpu_tuple_cost",
    "cursor_tuple_fraction",
    "deadlock_timeout",
    "debug_discard_caches",
    "debug_logical_replication_streaming",
    "debug_parallel_query",
    "debug_pretty_print",
    "debug_print_parse",
    "debug_print_plan",
    "debug_print_rewritten",
    "default_statistics_target",
    "default_table_access_method",
    "dynamic_library_path",
    "effective_cache_size",
    "effective_io_concurrency",
    "enable_async_append",
    "enable_bitmapscan",
    "enable_gathermerge",
    "enable_hashagg",
    "enable_hashjoin",
    "enable_incremental_sort",
    "enable_indexonlyscan",
    "enable_indexscan",
    "enable_material",
    "enable_memoize",
    "enable_mergejoin",
    "enable_nestloop",
    "enable_parallel_append",
    "enable_parallel_hash",
    "enable_partition_pruning",
    "enable_partitionwise_aggregate",
    "enable_partitionwise_join",
    "enable_presorted_aggregate",
    "enable_seqscan",
    "enable_sort",
    "enable_tidscan",
    "escape_string_warning",
    "exit_on_error",
    "extension_destdir",
    "from_collapse_limit",
    "geqo",
    "geqo_effort",
    "geqo_generations",
    "geqo_pool_size",
    "geqo_seed",
    "geqo_selection_bias",
    "geqo_threshold",
    "hash_mem_multiplier",
    "idle_in_transaction_session_timeout",
    "idle_session_timeout",
    "ignore_checksum_failure",
    "jit",
    "jit_above_cost",
    "jit_dump_bitcode",
    "jit_expressions",
    "jit_inline_above_cost",
    "jit_optimize_above_cost",
    "jit_tuple_deforming",
    "join_collapse_limit",
    "local_preload_libraries",
    "lock_timeout",
    "log_duration",
    "log_error_verbosity",
    "log_executor_stats",
    "log_lock_waits",
    "log_min_duration_sample",
    "log_min_duration_statement",
    "log_min_error_statement",
    "log_min_messages",
    "log_parameter_max_length",
    "log_parameter_max_length_on_error",
    "log_parser_stats",
    "log_planner_stats",
    "log_replication_commands",
    "log_statement",
    "log_statement_sample_rate",
    "log_statement_stats",
    "log_temp_files",
    "log_transaction_sample_rate",
    "logical_decoding_work_mem",
    "maintenance_io_concurrency",
    "maintenance_work_mem",
    "max_parallel_maintenance_workers",
    "max_parallel_workers",
    "max_parallel_workers_per_gather",
    "max_stack_depth",
    "min_parallel_index_scan_size",
    "min_parallel_table_scan_size",
    "parallel_leader_participation",
    "parallel_setup_cost",
    "parallel_tuple_cost",
    "password_encryption",
    "plan_cache_mode",
    "random_page_cost",
    "recursive_worktable_factor",
    "scram_iterations",
    "seq_page_cost",
    "session_preload_libraries",
    "standard_conforming_strings",
    "statement_timeout",
    "stats_fetch_consistency",
    "synchronous_commit",
    "tcp_keepalives_count",
    "tcp_keepalives_idle",
    "tcp_keepalives_interval",
    "tcp_user_timeout",
    "temp_buffers",
    "temp_file_limit",
    "trace_notify",
    "trace_sort",
    "track_activities",
    "track_counts",
    "track_functions",
    "track_io_timing",
    "track_wal_io_timing",
    "transaction_timeout",
    "update_process_title",
    "vacuum_buffer_usage_limit",
    "vacuum_cost_delay",
    "vacuum_cost_limit",
    "vacuum_cost_page_dirty",
    "vacuum_cost_page_hit",
    "vacuum_cost_page_miss",
    "wal_compression",
    "wal_consistency_checking",
    "wal_init_zero",
    "wal_recycle",
    "wal_sender_timeout",
    "wal_skip_threshold",
    "work_mem",
    "xmloption",
    "zero_damaged_pages",
];

/// True when `name` is one of [`RESULT_NEUTRAL_SETTINGS`], matched without case
/// sensitivity because a PostgreSQL parameter name is an identifier.
fn is_result_neutral_setting(name: &str) -> bool {
    RESULT_NEUTRAL_SETTINGS
        .binary_search_by(|neutral| {
            neutral
                .bytes()
                .map(|byte| byte.to_ascii_lowercase())
                .cmp(name.bytes().map(|byte| byte.to_ascii_lowercase()))
        })
        .is_ok()
}

/// True when `values` say the setting is being turned on. PostgreSQL accepts
/// `on`, `true`, `yes`, and `1` for a boolean parameter.
fn is_enabled(values: &[Expr]) -> bool {
    let [value] = values else { return false };
    let rendered = value.to_string();
    let rendered = rendered.trim_matches('\'');
    ["on", "true", "yes", "1"].iter().any(|enabled| enabled.eq_ignore_ascii_case(rendered))
}

/// Drops a `SET` of a setting listed in [`RESULT_NEUTRAL_SETTINGS`], refuses
/// every other form.
///
/// `standard_conforming_strings` is neutral only when turned ON, which is what
/// every `pg_dump` writes. `sqlparser` does not override
/// `supports_string_literal_backslash_escape`, so it always parses PostgreSQL
/// string literals by the standard-conforming rule and cannot be told
/// otherwise. Turning the parameter off therefore means the input was already
/// parsed under the wrong rule before reaching this translator, and reporting
/// it is the only thing left that helps.
fn translate_set(
    statement: &Statement,
    set: &Set,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    if let Set::SingleAssignment { variable, values, .. } = set
        && let Some(setting) = last_ident(variable)
    {
        let name = setting.value.as_str();
        if name.eq_ignore_ascii_case("standard_conforming_strings") && !is_enabled(values) {
            return Err(reject_unsupported_statement(
                statement,
                "Turning it off makes a backslash an escape inside an ordinary string literal, and \
                 the parser reading this input always applies the standard-conforming rule, so the \
                 literals have already been read the other way. Use E'' literals instead.",
            ));
        }
        if is_result_neutral_setting(name) {
            return Ok(drop_with_warning("SET", REASON_SET_NEUTRAL, emit));
        }
    }

    Err(reject_unsupported_statement(
        statement,
        "The PostgreSQL parameter carries query semantics, deciding how later statements resolve \
         names, whether policies and triggers apply, or how a value reads, and SQLite has no \
         counterpart, so dropping it could change what those statements do.",
    ))
}

/// A function definition emits nothing either way, and which outcome that is
/// depends on whether a trigger calls it.
///
/// A trigger function is realised: `create_trigger.rs` inlines its body into
/// every trigger that executes it, so the definition is consumed by the
/// pipeline rather than lost, and a warning would be a false report. Any other
/// function IS lost, and the host has to register a SQLite function of the same
/// name through the C API, the way this crate expects `current_setting` to be
/// registered for row level security, so that one is warned about.
///
/// The trigger names come from `populate_prewalk_catalogs`, since
/// `schema_statement_is_ignored` excludes `CREATE TRIGGER` from the translation
/// schema.
fn translate_create_function(
    create_function: &CreateFunction,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Vec<Statement> {
    if options.has_trigger_function_name(&last_ident_value_or_display(&create_function.name)) {
        return Vec::new();
    }
    drop_with_warning("CREATE FUNCTION", REASON_FUNCTION, emit)
}

/// `DISCARD PLANS` and `DISCARD SEQUENCES` throw away server caches SQLite does
/// not keep, so dropping them cannot change a result. `DISCARD ALL` and
/// `DISCARD TEMP` also destroy the session's temporary tables, which a later
/// statement can read, so those are refused.
fn translate_discard(
    statement: &Statement,
    object_type: DiscardObject,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<Statement>, Error> {
    match object_type {
        DiscardObject::PLANS | DiscardObject::SEQUENCES => {
            Ok(drop_with_warning("DISCARD", REASON_HINT, emit))
        }
        DiscardObject::ALL | DiscardObject::TEMP => {
            Err(reject_unsupported_statement(
                statement,
                "It destroys the session's temporary tables, and SQLite has no statement that does \
                 so, therefore a later statement reading one would behave differently. Drop the \
                 temporary tables by name instead.",
            ))
        }
    }
}

crate::traits::translator::impl_contextual_translator!(Statement => Vec<Statement>);
impl crate::traits::translator::TranslatorWithContext for Statement {
    #[allow(clippy::too_many_lines)]
    fn translate_with_warnings(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        let mut translated: Vec<Statement> = match self {
            Self::CreateTable(create_table) => {
                translate_create_table(create_table, schema, options, emit)?
            }
            Self::CreateIndex(create_index) => {
                match role_access_for_object_name(&create_index.table_name, schema, options)? {
                    RoleTableAccess::Allow => {
                        create_index.translate_with_warnings(schema, options, emit)?
                    }
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
                    emit,
                )?;
                statements
            }
            Self::Insert(insert) => {
                vec![insert.translate_with_warnings(schema, options, emit)?.into()]
            }
            Self::CreateView(create_view) => {
                translate_view_definition(create_view, schema, options, emit)?
            }
            // `ALTER VIEW v AS ...` redefines a view, which is what
            // `CREATE OR REPLACE VIEW` does, so it takes the same path. Every
            // other `ALTER VIEW` spelling PostgreSQL has, `OWNER TO`,
            // `SET SCHEMA`, and `RENAME TO`, fails in the parser before
            // reaching here, so there is no other shape to classify.
            Self::AlterView { name, columns, query, with_options } => {
                translate_view_definition(
                    &alter_view_as_create_view(name, columns, query, with_options),
                    schema,
                    options,
                    emit,
                )?
            }
            Self::Update(update) => {
                vec![Statement::Update(update.translate_with_warnings(schema, options, emit)?)]
            }
            Self::Delete(delete) => vec![delete.translate_with_warnings(schema, options, emit)?],
            Self::Query(query) => {
                vec![Statement::Query(Box::new(
                    query.translate_with_warnings(schema, options, emit)?,
                ))]
            }
            Self::If(if_stmt) => {
                let Some(if_condition) = &if_stmt.if_block.condition else {
                    // Guard-less: every branch's statements would be dropped,
                    // and those statements are the work the
                    // script asked for.
                    return Err(reject_unsupported_statement(
                        self,
                        "The IF block carries no condition, so its branches cannot be turned into \
             guarded statements.",
                    ));
                };

                let translated_if_condition =
                    if_condition.translate_with_warnings(schema, options, emit)?;
                let mut statements = Vec::new();
                append_guarded_statements(
                    &mut statements,
                    if_stmt.if_block.statements(),
                    &translated_if_condition,
                    schema,
                    options,
                    emit,
                )?;

                let mut prior_conditions = vec![translated_if_condition];

                for elseif_block in &if_stmt.elseif_blocks {
                    let Some(elseif_condition) = &elseif_block.condition else {
                        continue;
                    };
                    let translated_elseif_condition =
                        elseif_condition.translate_with_warnings(schema, options, emit)?;
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
                        emit,
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
                        emit,
                    )?;
                }

                statements
            }
            // Statements that are already SQLite's own syntax, passed through
            // unchanged. A pragma name is a SQLite setting and an attached alias is
            // a database handle, so neither is a schema-qualified object to
            // normalise. A savepoint name needs no normalising either, and
            // `ROLLBACK TO SAVEPOINT` is SQLite's own spelling.
            Self::Savepoint { .. }
            | Self::ReleaseSavepoint { .. }
            | Self::Pragma { .. }
            | Self::AttachDatabase { .. } => vec![self.clone()],
            Self::StartTransaction {
                modes, transaction, modifier, statements, exception, ..
            } => {
                translate_start_transaction(
                    self,
                    modes,
                    transaction.as_ref(),
                    *modifier,
                    statements,
                    exception.as_deref(),
                    emit,
                )?
            }
            Self::Commit { chain, end, modifier } => {
                translate_commit(self, *chain, *end, *modifier)?
            }
            Self::Rollback { chain, savepoint } => translate_rollback(*chain, savepoint.as_ref()),
            Self::Vacuum(vacuum) => translate_vacuum(vacuum, emit),
            // DROP TABLE/VIEW/INDEX - translate to SQLite (strip CASCADE/RESTRICT)
            Self::Drop { object_type, if_exists, names, cascade, .. } => {
                match object_type {
                    // SQLite supports these object types
                    ObjectType::Table | ObjectType::View | ObjectType::Index => {
                        let normalized_names = names
                            .iter()
                            .map(|name| {
                                normalize_schema_qualified_object_name_for_sqlite(schema, name)
                            })
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
                    // A schema, role, or type never reached the output, so
                    // dropping the statement removes nothing that exists. The
                    // exceptions are the two that would destroy tables: a
                    // cascading DROP SCHEMA removes every table in the schema,
                    // and DROP DATABASE removes the lot.
                    ObjectType::Schema if *cascade => {
                        return Err(reject_unsupported_statement(
                            self,
                            "A cascading DROP SCHEMA deletes every table in the schema, and the \
                 translated tables carry no schema, so which tables it means cannot be \
                 determined. Drop the tables by name instead.",
                        ));
                    }
                    ObjectType::Database | ObjectType::MaterializedView => {
                        return Err(reject_unsupported_statement(
                            self,
                            "SQLite has neither, and the matching CREATE is refused, so nothing of \
                 the kind can exist in the output.",
                        ));
                    }
                    ObjectType::Schema
                    | ObjectType::Role
                    | ObjectType::User
                    | ObjectType::Sequence
                    | ObjectType::Type
                    | ObjectType::Collation
                    | ObjectType::Stage
                    | ObjectType::Stream
                    | ObjectType::Warehouse => {
                        drop_with_warning(
                            drop_label(*object_type),
                            REASON_DROP_OF_ABSENT_OBJECT,
                            emit,
                        )
                    }
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
            // Outcome 3, warn and drop. Grouped by the reason each drop cannot
            // change a result.
            Self::LISTEN { .. } => drop_with_warning("LISTEN", REASON_PUB_SUB, emit),
            Self::UNLISTEN { .. } => drop_with_warning("UNLISTEN", REASON_PUB_SUB, emit),
            Self::NOTIFY { .. } => drop_with_warning("NOTIFY", REASON_PUB_SUB, emit),
            Self::CreateRole(_) => drop_with_warning("CREATE ROLE", REASON_ACCESS_CONTROL, emit),
            Self::CreateUser(_) => drop_with_warning("CREATE USER", REASON_ACCESS_CONTROL, emit),
            Self::AlterRole { .. } => drop_with_warning("ALTER ROLE", REASON_ACCESS_CONTROL, emit),
            Self::AlterUser(_) => drop_with_warning("ALTER USER", REASON_ACCESS_CONTROL, emit),
            Self::Grant(_) => drop_with_warning("GRANT", REASON_ACCESS_CONTROL, emit),
            Self::Revoke(_) => drop_with_warning("REVOKE", REASON_ACCESS_CONTROL, emit),
            Self::Deny { .. } => drop_with_warning("DENY", REASON_ACCESS_CONTROL, emit),
            Self::CreateType { .. } => {
                drop_with_warning("CREATE TYPE", REASON_TYPE_DEFINITION, emit)
            }
            Self::AlterType(_) => drop_with_warning("ALTER TYPE", REASON_TYPE_DEFINITION, emit),
            Self::CreateDomain(_) => {
                drop_with_warning("CREATE DOMAIN", REASON_TYPE_DEFINITION, emit)
            }
            Self::DropDomain { .. } => {
                drop_with_warning("DROP DOMAIN", REASON_TYPE_DEFINITION, emit)
            }
            Self::CreateCollation(_) => {
                drop_with_warning("CREATE COLLATION", REASON_HOST_REGISTERED, emit)
            }
            Self::AlterCollation(_) => {
                drop_with_warning("ALTER COLLATION", REASON_HOST_REGISTERED, emit)
            }
            Self::CreateFunction(create_function) => {
                translate_create_function(create_function, options, emit)
            }
            Self::DropFunction { .. } => drop_with_warning("DROP FUNCTION", REASON_FUNCTION, emit),
            Self::AlterFunction(_) => {
                drop_with_warning("ALTER FUNCTION", REASON_ACCESS_CONTROL, emit)
            }
            Self::CreateServer(_) => drop_with_warning("CREATE SERVER", REASON_FOREIGN_DATA, emit),
            Self::CreateConnector(_) => {
                drop_with_warning("CREATE CONNECTOR", REASON_FOREIGN_DATA, emit)
            }
            Self::AlterConnector { .. } => {
                drop_with_warning("ALTER CONNECTOR", REASON_FOREIGN_DATA, emit)
            }
            Self::DropConnector { .. } => {
                drop_with_warning("DROP CONNECTOR", REASON_FOREIGN_DATA, emit)
            }
            Self::CreateSecret { .. } => {
                drop_with_warning("CREATE SECRET", REASON_FOREIGN_DATA, emit)
            }
            Self::DropSecret { .. } => drop_with_warning("DROP SECRET", REASON_FOREIGN_DATA, emit),
            Self::CreateExtension(_) => {
                drop_with_warning("CREATE EXTENSION", REASON_EXTENSION, emit)
            }
            Self::DropExtension { .. } => {
                drop_with_warning("DROP EXTENSION", REASON_EXTENSION, emit)
            }
            Self::CreateSequence { .. } => {
                drop_with_warning("CREATE SEQUENCE", REASON_SEQUENCE, emit)
            }
            Self::Comment { .. } => drop_with_warning("COMMENT", REASON_INTROSPECTION, emit),
            Self::ExplainTable { .. } => drop_with_warning("DESCRIBE", REASON_INTROSPECTION, emit),
            Self::ShowVariable { .. } => drop_with_warning("SHOW", REASON_INTROSPECTION, emit),
            Self::ShowVariables { .. } => {
                drop_with_warning("SHOW VARIABLES", REASON_INTROSPECTION, emit)
            }
            Self::ShowStatus { .. } => drop_with_warning("SHOW STATUS", REASON_INTROSPECTION, emit),
            Self::ShowTables { .. } => drop_with_warning("SHOW TABLES", REASON_INTROSPECTION, emit),
            Self::ShowViews { .. } => drop_with_warning("SHOW VIEWS", REASON_INTROSPECTION, emit),
            Self::ShowColumns { .. } => {
                drop_with_warning("SHOW COLUMNS", REASON_INTROSPECTION, emit)
            }
            Self::ShowCreate { .. } => drop_with_warning("SHOW CREATE", REASON_INTROSPECTION, emit),
            Self::ShowSchemas { .. } => {
                drop_with_warning("SHOW SCHEMAS", REASON_INTROSPECTION, emit)
            }
            Self::ShowDatabases { .. } => {
                drop_with_warning("SHOW DATABASES", REASON_INTROSPECTION, emit)
            }
            Self::ShowCatalogs { .. } => {
                drop_with_warning("SHOW CATALOGS", REASON_INTROSPECTION, emit)
            }
            Self::ShowCharset { .. } => {
                drop_with_warning("SHOW CHARSET", REASON_INTROSPECTION, emit)
            }
            Self::ShowCollation { .. } => {
                drop_with_warning("SHOW COLLATION", REASON_INTROSPECTION, emit)
            }
            Self::ShowFunctions { .. } => {
                drop_with_warning("SHOW FUNCTIONS", REASON_INTROSPECTION, emit)
            }
            Self::ShowObjects(_) => drop_with_warning("SHOW OBJECTS", REASON_INTROSPECTION, emit),
            Self::ShowProcessList { .. } => {
                drop_with_warning("SHOW PROCESSLIST", REASON_INTROSPECTION, emit)
            }
            Self::Kill { .. } => drop_with_warning("KILL", REASON_ADMINISTRATION, emit),
            Self::WaitFor(_) => drop_with_warning("WAITFOR", REASON_WAIT, emit),
            Self::Msck(_) => drop_with_warning("MSCK", REASON_ADMINISTRATION, emit),
            Self::Flush { .. } => drop_with_warning("FLUSH", REASON_ADMINISTRATION, emit),
            Self::Install { .. } => drop_with_warning("INSTALL", REASON_ADMINISTRATION, emit),
            Self::Load { .. } => drop_with_warning("LOAD", REASON_ADMINISTRATION, emit),
            Self::CreateWarehouse(_) => {
                drop_with_warning("CREATE WAREHOUSE", REASON_ADMINISTRATION, emit)
            }
            Self::CreateStage { .. } => {
                drop_with_warning("CREATE STAGE", REASON_ADMINISTRATION, emit)
            }
            Self::CreateFileFormat { .. } => {
                drop_with_warning("CREATE FILE FORMAT", REASON_ADMINISTRATION, emit)
            }
            Self::Cache { .. } => drop_with_warning("CACHE TABLE", REASON_HINT, emit),
            Self::UNCache { .. } => drop_with_warning("UNCACHE TABLE", REASON_HINT, emit),
            Self::OptimizeTable { .. } => drop_with_warning("OPTIMIZE TABLE", REASON_HINT, emit),
            Self::Lock { .. } => drop_with_warning("LOCK", REASON_HINT, emit),
            Self::LockTables { .. } => drop_with_warning("LOCK TABLES", REASON_HINT, emit),
            Self::UnlockTables => drop_with_warning("UNLOCK TABLES", REASON_HINT, emit),
            Statement::AlterTable(alter_table) => {
                translate_alter_table(alter_table, schema, options, emit)?
            }
            Statement::RenameTable(renames) => return Err(reject_rename_table(renames)),
            Statement::Truncate(truncate) => translate_truncate(truncate, schema, options, emit)?,
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
            // Already SQLite's own syntax, so these pass through. A table name is
            // normalised the way every other table reference is, since a
            // schema-qualified PostgreSQL name has no SQLite counterpart.
            Statement::Analyze(analyze) => {
                let mut analyze = analyze.clone();
                if let Some(table_name) = &analyze.table_name {
                    analyze.table_name = Some(normalize_schema_qualified_object_name_for_sqlite(
                        schema, table_name,
                    )?);
                }
                vec![Statement::Analyze(analyze)]
            }
            Statement::CreateVirtualTable { name, if_not_exists, module_name, module_args } => {
                vec![Statement::CreateVirtualTable {
                    name: normalize_schema_qualified_object_name_for_sqlite(schema, name)?,
                    if_not_exists: *if_not_exists,
                    module_name: module_name.clone(),
                    module_args: module_args.clone(),
                }]
            }
            Statement::Explain { analyze, statement, .. } => {
                translate_explain(*analyze, statement, schema, options, emit)?
            }
            // Outcome 4, consumed by the translation schema. Each of these is
            // realised elsewhere, so emitting nothing is the complete
            // translation. A warning here would be false.
            //
            // The policy statements drive the row level security view and
            // trigger set built from the final schema state (`rls.rs`,
            // `filter_policies`). The schema statements make a qualified name
            // resolvable (`object_name.rs`, `schema_resolves`) and SQLite has no
            // schema for the name to keep, so the qualifier is stripped instead.
            Self::CreatePolicy(_)
            | Self::AlterPolicy(_)
            | Self::DropPolicy { .. }
            | Self::CreateSchema { .. }
            | Self::AlterSchema(_) => Vec::new(),
            // Outcome 2, hard error, grouped by reason.
            data_movement_patterns!() => {
                return Err(reject_unsupported_statement(
                    self,
                    "SQLite has no statement that moves data between the database and a file or \
         stage, so the rows the statement carries would be lost.",
                ));
            }
            control_flow_patterns!() => {
                return Err(reject_unsupported_statement(
                    self,
                    "SQLite has no procedural control flow: WHILE loops, LOOP, FOR, and \
         procedural CASE have no SQL equivalent and cannot be emitted. Rewrite \
         as a set-based statement.",
                ));
            }
            cursor_patterns!() => {
                return Err(reject_unsupported_statement(
                    self,
                    "SQLite has no server-side cursors: iteration happens in the host through \
         sqlite3_step, which has no SQL spelling, so the work the cursor performs \
         cannot be emitted. Rewrite the loop as a set-based statement.",
                ));
            }
            procedural_patterns!() => {
                return Err(reject_unsupported_statement(
                    self,
                    "SQLite has no stored procedures, so the body would be lost and a call to it \
         would perform nothing. Inline the statements at each call site.",
                ));
            }
            session_state_patterns!() => {
                return Err(reject_unsupported_statement(
                    self,
                    "The statement changes session state that decides how later statements resolve \
         names or behave, and SQLite has no equivalent, so dropping it could change \
         what those statements do.",
                ));
            }
            extensibility_patterns!() => {
                return Err(reject_unsupported_statement(
                    self,
                    "SQLite cannot define an operator, an operator class or family, or a text \
         search configuration, and an unrecognised operator is emitted unchanged, so \
         dropping this definition would leave the emitted SQL to fail at run time.",
                ));
            }
            alter_in_place_patterns!() => {
                return Err(reject_unsupported_statement(
                    self,
                    "SQLite cannot rename an index in place: it has to be dropped and recreated, \
         which needs its current definition. Write the DROP and the CREATE out \
         instead.",
                ));
            }
            database_level_patterns!() => {
                return Err(reject_unsupported_statement(
                    self,
                    "A SQLite database is a file rather than an object inside a server, so it is \
         created by opening it and joined to a session with ATTACH.",
                ));
            }
            Self::Set(set) => translate_set(self, set, emit)?,
            Self::Discard { object_type } => translate_discard(self, *object_type, emit)?,
        };

        // PostgreSQL numbered parameters (`$N`) become SQLite `?N`
        // placeholders, preserving the number so the bind index
        // survives a round trip. Only DML carries placeholders, so DDL
        // output skips the walk.
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

    use super::{
        RESULT_NEUTRAL_SETTINGS, inject_condition, is_result_neutral_setting,
        translate_create_table_for_role,
    };
    use crate::prelude::{Pg2SqliteOptions, Translator};

    /// `is_result_neutral_setting` binary searches the table, so an unsorted or
    /// duplicated entry would make a lookup miss silently.
    #[test]
    fn result_neutral_settings_are_sorted_and_unique() {
        for pair in RESULT_NEUTRAL_SETTINGS.windows(2) {
            assert!(pair[0] < pair[1], "{} must sort before {}", pair[0], pair[1]);
        }
    }

    /// Both ends of the table and the case-insensitive match, since a parameter
    /// name is an identifier and `SET WORK_MEM` is the same setting.
    #[test]
    fn result_neutral_settings_are_found_at_either_end_and_ignoring_case() {
        assert!(is_result_neutral_setting("allow_in_place_tablespaces"));
        assert!(is_result_neutral_setting("zero_damaged_pages"));
        assert!(is_result_neutral_setting("WORK_MEM"));
        assert!(!is_result_neutral_setting("search_path"));
        assert!(!is_result_neutral_setting("gin_fuzzy_search_limit"));
        assert!(!is_result_neutral_setting("work_me"));
    }

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

    /// Each base condition has a top level OR, so a guard appended without
    /// parentheses would bind to the last disjunct alone. `tests/
    /// test_condition_injection.rs` covers the same case by row count.
    #[test]
    fn inject_condition_updates_insert_update_and_delete_statements() {
        let condition = Expr::Value(ValueWithSpan {
            value: Value::Boolean(true),
            span: sqlparser::tokenizer::Span::empty(),
        });

        for sql in [
            "INSERT INTO logs(id) SELECT id FROM users WHERE active = 1 OR admin = 1",
            "UPDATE users SET active = 0 WHERE id = 1 OR id = 2",
            "DELETE FROM users WHERE id = 1 OR id = 2",
        ] {
            let mut statement =
                Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap().into_iter().next().unwrap();
            inject_condition(&mut statement, condition.clone()).unwrap();
            let injected = statement.to_string().to_uppercase();
            assert!(injected.contains("AND TRUE"), "unexpected SQL: {statement}");
            assert!(
                injected.contains("OR ID = 2) AND TRUE")
                    || injected.contains("ADMIN = 1) AND TRUE"),
                "the guard must bind to the whole condition: {statement}"
            );
        }
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
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
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

        let schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, "CREATE TABLE users(id INT);").unwrap(),
            "test".to_string(),
        )
        .unwrap();
        let options = crate::options::TranslationContext::from_owned(Pg2SqliteOptions::default());
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
        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_session_user_role("app_user"),
        );
        let missing_table = parse_create_table("CREATE TABLE docs(id INTEGER PRIMARY KEY)");
        let missing =
            translate_create_table_for_role(&missing_table, &missing_schema, &options, &mut |_| {})
                .unwrap();
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
        let readonly = translate_create_table_for_role(
            &readonly_table,
            &readonly_schema,
            &options,
            &mut |_| {},
        )
        .unwrap();
        let readonly = readonly.expect("readonly path should return statements");
        assert!(!readonly.is_empty(), "a read-only table emits at least one statement");
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
        let writable = translate_create_table_for_role(
            &writable_table,
            &writable_schema,
            &options,
            &mut |_| {},
        )
        .unwrap();
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

        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_session_user_role("app_user"),
        );
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

        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_session_user_role("app_user"),
        );
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

        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_session_user_role("app_user"),
        );
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

        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_session_user_role("app_user"),
        );
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

        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_session_user_role("missing_role"),
        );

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

        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_session_user_role("app_user"),
        );
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

        let options = crate::options::TranslationContext::from_owned(
            Pg2SqliteOptions::default().with_session_user_role("app_user"),
        );
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
