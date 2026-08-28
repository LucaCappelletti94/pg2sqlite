//! Parity for plpgsql functions and the triggers that call them.
//!
//! Five fixtures, each with at least one trigger. The DML that fires each
//! trigger runs on both engines and the side-effect tables are compared
//! afterwards. Trigger effects are invisible unless the storage is read, so
//! every assertion reads the tables the trigger touched.

use std::sync::atomic::{AtomicU64, Ordering};

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};
use postgres_harness::Outcome;
use rosetta_uuid::Uuid;

use crate::{helpers, postgres_harness};

/// Translation options shared across all five scenarios.
fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_uuid_function_name("uuidv7")
}

/// Options for a fixture that keys its tables on `TEXT`.
///
/// The suite's shared `uuidv7` answers sixteen bytes, which a `TEXT` column
/// under `STRICT` refuses, so such a fixture needs a generator that answers
/// text. PostgreSQL has the same distinction and coerces its own `uuid` into
/// the text column, so pointing the translation at a text generator is what
/// makes the two engines comparable rather than a way of avoiding a failure.
fn text_uuid_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_v7_function_name("uuid_text")
        .with_uuid_function_name("uuid_text")
}

diesel::define_sql_function! {
    /// A uuid rendered as text, registered on the connection below.
    fn uuid_text() -> diesel::sql_types::Text;
}

/// Answers a distinct uuid per call, since the fixture keys a history table on
/// it and PostgreSQL's own generator never repeats. Counted rather than random
/// so a failure names a value that can be read back.
fn uuid_text_impl() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("00000000-0000-7000-8000-{:012x}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Translates a fixture and applies the DDL to a fresh in-memory SQLite
/// connection. Raw SQL is the right tool here: the translator emits
/// multi-statement DDL with dollar-quoted bodies that the typed DSL cannot
/// express.
fn apply_to_sqlite(fixture: &str) -> SqliteConnection {
    apply_to_sqlite_with(fixture, &options())
}

fn apply_to_sqlite_with(fixture: &str, options: &Pg2SqliteOptions) -> SqliteConnection {
    let translated = Pg2Sqlite::default()
        .sql(fixture)
        .expect("parse fixture")
        .translate(options)
        .expect("translate fixture");
    let mut conn = establish_connection();
    uuid_text_utils::register_impl(&conn, uuid_text_impl).expect("register uuid_text");
    for stmt in &translated {
        // DDL migration: raw SQL is the correct form here.
        diesel::sql_query(stmt.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
    }
    conn
}

// ══════════════════════════════════════════════════════════════
// trigger_elsif_else: IF / ELSIF / ELSE branch coverage
// ══════════════════════════════════════════════════════════════

const ELSIF_ELSE: &str = include_str!("../fixtures/trigger_elsif_else.sql");

mod elsif_schema {
    diesel::table! {
        audit_log (id) {
            id -> Integer,
            action -> Text,
            severity -> Text,
        }
    }
    diesel::table! {
        events (id) {
            id -> Integer,
            event_type -> Text,
            data -> Nullable<Text>,
        }
    }
}

#[derive(Debug)]
enum ElsifStep {
    InsertEvent { event_type: &'static str },
    ReadAuditLog,
}

macro_rules! elsif_runner {
    ($name:ident, $conn:ty) => {
        struct $name {
            connection: $conn,
        }
        impl $name {
            fn step(&mut self, step: &ElsifStep) -> Outcome {
                use self::elsif_schema::{audit_log, events};
                match step {
                    ElsifStep::InsertEvent { event_type } => {
                        Outcome::of(
                            &diesel::insert_into(events::table)
                                .values((
                                    events::event_type.eq(*event_type),
                                    events::data.eq(Option::<String>::None),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    ElsifStep::ReadAuditLog => {
                        let rows: Vec<(String, String)> = audit_log::table
                            .select((audit_log::action, audit_log::severity))
                            .order((audit_log::action.asc(), audit_log::severity.asc()))
                            .load(&mut self.connection)
                            .expect("read audit_log");
                        Outcome::Rows(rows.into_iter().map(|(a, s)| format!("{a}:{s}")).collect())
                    }
                }
            }
        }
    };
}

elsif_runner!(ElsifPgRunner, PgConnection);
elsif_runner!(ElsifSqliteRunner, SqliteConnection);

/// All four branches: error (high), warning (medium), info (low), else
/// (unknown). Each is driven once and the audit_log is read after each insert.
fn elsif_scenario() -> Vec<ElsifStep> {
    vec![
        ElsifStep::InsertEvent { event_type: "error" },
        ElsifStep::ReadAuditLog,
        ElsifStep::InsertEvent { event_type: "warning" },
        ElsifStep::ReadAuditLog,
        ElsifStep::InsertEvent { event_type: "info" },
        ElsifStep::ReadAuditLog,
        ElsifStep::InsertEvent { event_type: "unexpected" },
        ElsifStep::ReadAuditLog,
    ]
}

/// Both engines route each event_type through the same IF/ELSIF/ELSE branch
/// and write the same severity to audit_log.
///
/// Proof that the comparison detects a mistranslation: temporarily changing
/// the expected ReadAuditLog result to Outcome::Refused caused the assertion
/// to fail with "trigger_elsif_else disagrees on ReadAuditLog", confirming
/// the comparison catches a wrong branch.
#[test]
fn trigger_elsif_else_engines_agree() {
    let mut pg = {
        let mut conn = postgres_harness::fresh_database();
        postgres_harness::apply(&mut conn, ELSIF_ELSE).expect("apply elsif_else to pg");
        ElsifPgRunner { connection: conn }
    };
    let mut sq = ElsifSqliteRunner { connection: apply_to_sqlite(ELSIF_ELSE) };

    for step in elsif_scenario() {
        let expected = pg.step(&step);
        let actual = sq.step(&step);
        assert_eq!(actual, expected, "trigger_elsif_else disagrees on {step:?}");
    }
}

// ══════════════════════════════════════════════════════════════
// trigger_issue: chained triggers across three tables
// ══════════════════════════════════════════════════════════════

const TRIGGER_ISSUE: &str = include_str!("../fixtures/trigger_issue.sql");

mod trigger_issue_schema {
    diesel::table! {
        procedure_template_asset_models (id) {
            id -> Integer,
            name -> Nullable<Text>,
            procedure_template_id -> Nullable<Integer>,
            based_on_id -> Nullable<Integer>,
            asset_model_id -> Nullable<Integer>,
        }
    }
    diesel::table! {
        parent_procedure_templates (parent_id, child_id) {
            parent_id -> Integer,
            child_id -> Integer,
        }
    }
    diesel::table! {
        next_procedure_templates (parent_id, predecessor_id, successor_id) {
            parent_id -> Integer,
            predecessor_id -> Integer,
            successor_id -> Integer,
        }
    }
}

#[derive(Debug)]
enum TriggerIssueStep {
    SeedAssetModel,
    InsertNextTemplate,
    ReadParentTemplates,
    CountAssetModels,
}

macro_rules! trigger_issue_runner {
    ($name:ident, $conn:ty) => {
        struct $name {
            connection: $conn,
        }
        impl $name {
            fn step(&mut self, step: &TriggerIssueStep) -> Outcome {
                use self::trigger_issue_schema::{
                    next_procedure_templates, parent_procedure_templates,
                    procedure_template_asset_models,
                };
                match step {
                    TriggerIssueStep::SeedAssetModel => {
                        Outcome::of(
                            &diesel::insert_into(procedure_template_asset_models::table)
                                .values((
                                    procedure_template_asset_models::name.eq("tool"),
                                    procedure_template_asset_models::procedure_template_id
                                        .eq(10_i32),
                                    procedure_template_asset_models::asset_model_id.eq(99_i32),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    TriggerIssueStep::InsertNextTemplate => {
                        Outcome::of(
                            &diesel::insert_into(next_procedure_templates::table)
                                .values((
                                    next_procedure_templates::parent_id.eq(1_i32),
                                    next_procedure_templates::predecessor_id.eq(10_i32),
                                    next_procedure_templates::successor_id.eq(20_i32),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    TriggerIssueStep::ReadParentTemplates => {
                        let rows: Vec<(i32, i32)> = parent_procedure_templates::table
                            .select((
                                parent_procedure_templates::parent_id,
                                parent_procedure_templates::child_id,
                            ))
                            .order((
                                parent_procedure_templates::parent_id.asc(),
                                parent_procedure_templates::child_id.asc(),
                            ))
                            .load(&mut self.connection)
                            .expect("read parent_procedure_templates");
                        Outcome::Rows(rows.into_iter().map(|(p, c)| format!("{p}:{c}")).collect())
                    }
                    TriggerIssueStep::CountAssetModels => {
                        let count = procedure_template_asset_models::table
                            .count()
                            .get_result::<i64>(&mut self.connection)
                            .expect("count asset models");
                        Outcome::Count(count)
                    }
                }
            }
        }
    };
}

trigger_issue_runner!(TriggerIssuePgRunner, PgConnection);
trigger_issue_runner!(TriggerIssueSqliteRunner, SqliteConnection);

fn trigger_issue_scenario() -> Vec<TriggerIssueStep> {
    vec![
        TriggerIssueStep::SeedAssetModel,
        TriggerIssueStep::InsertNextTemplate,
        TriggerIssueStep::ReadParentTemplates,
        TriggerIssueStep::CountAssetModels,
    ]
}

/// Trigger 1 on next_procedure_templates fires and populates
/// parent_procedure_templates with two rows. Trigger 2 then fires once per
/// parent_procedure_templates insert and copies asset models from child to
/// parent procedure. Both trigger chains run on both engines.
#[test]
fn trigger_issue_engines_agree() {
    let mut pg = {
        let mut conn = postgres_harness::fresh_database();
        postgres_harness::apply(&mut conn, TRIGGER_ISSUE).expect("apply trigger_issue to pg");
        TriggerIssuePgRunner { connection: conn }
    };
    let mut sq = TriggerIssueSqliteRunner { connection: apply_to_sqlite(TRIGGER_ISSUE) };

    for step in trigger_issue_scenario() {
        let expected = pg.step(&step);
        let actual = sq.step(&step);
        assert_eq!(actual, expected, "trigger_issue disagrees on {step:?}");
    }
}

// ══════════════════════════════════════════════════════════════
// trigger_uuid_insert: trigger writes a history row on todo INSERT
// ══════════════════════════════════════════════════════════════

const UUID_INSERT: &str = include_str!("../fixtures/trigger_uuid_insert.sql");

mod uuid_insert_schema {
    diesel::table! {
        todos (id) {
            id -> Text,
            title -> Text,
            created_at -> Nullable<Text>,
        }
    }
    diesel::table! {
        todo_history (id) {
            id -> Text,
            todo_id -> Text,
            action -> Text,
            created_at -> Nullable<Text>,
        }
    }
}

#[derive(Debug)]
enum UuidInsertStep {
    InsertTodo { id: &'static str, title: &'static str },
    CountHistory,
    ReadHistoryActions,
}

macro_rules! uuid_insert_runner {
    ($name:ident, $conn:ty) => {
        struct $name {
            connection: $conn,
        }
        impl $name {
            fn step(&mut self, step: &UuidInsertStep) -> Outcome {
                use self::uuid_insert_schema::{todo_history, todos};
                match step {
                    UuidInsertStep::InsertTodo { id, title } => {
                        Outcome::of(
                            &diesel::insert_into(todos::table)
                                .values((todos::id.eq(*id), todos::title.eq(*title)))
                                .execute(&mut self.connection),
                        )
                    }
                    UuidInsertStep::CountHistory => {
                        let count = todo_history::table
                            .count()
                            .get_result::<i64>(&mut self.connection)
                            .expect("count todo_history");
                        Outcome::Count(count)
                    }
                    UuidInsertStep::ReadHistoryActions => {
                        let actions: Vec<String> = todo_history::table
                            .select(todo_history::action)
                            .order(todo_history::action.asc())
                            .load(&mut self.connection)
                            .expect("read history actions");
                        Outcome::Rows(actions)
                    }
                }
            }
        }
    };
}

uuid_insert_runner!(UuidInsertPgRunner, PgConnection);
uuid_insert_runner!(UuidInsertSqliteRunner, SqliteConnection);

fn uuid_insert_scenario() -> Vec<UuidInsertStep> {
    vec![
        UuidInsertStep::InsertTodo { id: "todo-a", title: "first" },
        UuidInsertStep::CountHistory,
        UuidInsertStep::ReadHistoryActions,
        UuidInsertStep::InsertTodo { id: "todo-b", title: "second" },
        UuidInsertStep::CountHistory,
    ]
}

/// FINDING: this test fails. The trigger body uses a DECLARE variable
/// (`DECLARE new_id TEXT; new_id := gen_random_uuid();`) that pg2sqlite does
/// not inline into the SQLite trigger. The resulting SQLite trigger references
/// an unresolved identifier, so the INSERT into todos fires the trigger, the
/// trigger fails, and SQLite rolls back the whole INSERT. PostgreSQL accepts
/// the same INSERT. The step that disagrees is InsertTodo, outcome left:
/// Refused, right: Accepted. Left failing per the comparison contract.
#[test]
fn trigger_uuid_insert_engines_agree() {
    let mut pg = {
        let mut conn = postgres_harness::fresh_database();
        postgres_harness::apply(&mut conn, UUID_INSERT).expect("apply uuid_insert to pg");
        UuidInsertPgRunner { connection: conn }
    };
    let mut sq = UuidInsertSqliteRunner {
        connection: apply_to_sqlite_with(UUID_INSERT, &text_uuid_options()),
    };

    for step in uuid_insert_scenario() {
        let expected = pg.step(&step);
        let actual = sq.step(&step);
        assert_eq!(actual, expected, "trigger_uuid_insert disagrees on {step:?}");
    }
}

// ══════════════════════════════════════════════════════════════
// trigger_with_recursive: WITH RECURSIVE rebuilds ancestors on INSERT
// ══════════════════════════════════════════════════════════════

const WITH_RECURSIVE: &str = include_str!("../fixtures/trigger_with_recursive.sql");

mod recursive_schema {
    diesel::table! {
        tree_nodes (id) {
            id -> Integer,
            parent_id -> Nullable<Integer>,
            name -> Text,
        }
    }
    diesel::table! {
        node_ancestors (node_id, ancestor_id) {
            node_id -> Integer,
            ancestor_id -> Integer,
        }
    }
}

#[derive(Debug)]
enum RecursiveStep {
    InsertNode { parent_id: Option<i32>, name: &'static str },
    ReadAncestors,
}

macro_rules! recursive_runner {
    ($name:ident, $conn:ty) => {
        struct $name {
            connection: $conn,
        }
        impl $name {
            fn step(&mut self, step: &RecursiveStep) -> Outcome {
                use self::recursive_schema::{node_ancestors, tree_nodes};
                match step {
                    RecursiveStep::InsertNode { parent_id, name } => {
                        Outcome::of(
                            &diesel::insert_into(tree_nodes::table)
                                .values((
                                    tree_nodes::parent_id.eq(*parent_id),
                                    tree_nodes::name.eq(*name),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    RecursiveStep::ReadAncestors => {
                        let rows: Vec<(i32, i32)> = node_ancestors::table
                            .select((node_ancestors::node_id, node_ancestors::ancestor_id))
                            .order((
                                node_ancestors::node_id.asc(),
                                node_ancestors::ancestor_id.asc(),
                            ))
                            .load(&mut self.connection)
                            .expect("read node_ancestors");
                        Outcome::Rows(rows.into_iter().map(|(n, a)| format!("{n}:{a}")).collect())
                    }
                }
            }
        }
    };
}

recursive_runner!(RecursivePgRunner, PgConnection);
recursive_runner!(RecursiveSqliteRunner, SqliteConnection);

fn recursive_scenario() -> Vec<RecursiveStep> {
    vec![
        RecursiveStep::InsertNode { parent_id: None, name: "root" },
        RecursiveStep::ReadAncestors,
        RecursiveStep::InsertNode { parent_id: Some(1), name: "child" },
        RecursiveStep::ReadAncestors,
        RecursiveStep::InsertNode { parent_id: Some(2), name: "grandchild" },
        RecursiveStep::ReadAncestors,
    ]
}

/// The trigger rebuilds the ancestors table on each node INSERT using WITH
/// RECURSIVE. After inserting root (no ancestors), child (ancestor: root), and
/// grandchild (ancestors: child and root), the exact (node_id, ancestor_id)
/// rows are compared across engines.
#[test]
fn trigger_with_recursive_engines_agree() {
    let mut pg = {
        let mut conn = postgres_harness::fresh_database();
        postgres_harness::apply(&mut conn, WITH_RECURSIVE).expect("apply with_recursive to pg");
        RecursivePgRunner { connection: conn }
    };
    let mut sq = RecursiveSqliteRunner { connection: apply_to_sqlite(WITH_RECURSIVE) };

    for step in recursive_scenario() {
        let expected = pg.step(&step);
        let actual = sq.step(&step);
        assert_eq!(actual, expected, "trigger_with_recursive disagrees on {step:?}");
    }
}

// ══════════════════════════════════════════════════════════════
// groups: recursive membership propagation in both directions
// ══════════════════════════════════════════════════════════════

const GROUPS: &str = include_str!("../fixtures/groups.sql");

mod groups_schema {
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        owners (id) {
            id -> Uuid,
        }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        groups (id) {
            id -> Uuid,
            parent_group_id -> Nullable<Uuid>,
            name -> Text,
        }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        users (id) {
            id -> Uuid,
            name -> Text,
        }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        group_memberships (id) {
            id -> Uuid,
            group_id -> Uuid,
            user_id -> Uuid,
        }
    }
}

fn grandparent_group() -> Uuid {
    Uuid::from([0x10; 16])
}
fn parent_group() -> Uuid {
    Uuid::from([0x20; 16])
}
fn child_group() -> Uuid {
    Uuid::from([0x30; 16])
}
fn alice() -> Uuid {
    Uuid::from([0xa0; 16])
}

#[derive(Debug)]
enum GroupsStep {
    InsertRootGroup { id: Uuid, name: &'static str },
    InsertChildGroup { id: Uuid, parent_id: Uuid, name: &'static str },
    InsertUser { id: Uuid, name: &'static str },
    AddToGroup { group_id: Uuid, user_id: Uuid },
    RemoveFromGroup { group_id: Uuid, user_id: Uuid },
    CountMemberships,
}

macro_rules! groups_runner {
    ($name:ident, $conn:ty) => {
        struct $name {
            connection: $conn,
        }
        impl $name {
            fn step(&mut self, step: &GroupsStep) -> Outcome {
                use self::groups_schema::{group_memberships, groups, users};
                match step {
                    GroupsStep::InsertRootGroup { id, name } => {
                        Outcome::of(
                            &diesel::insert_into(groups::table)
                                .values((groups::id.eq(*id), groups::name.eq(*name)))
                                .execute(&mut self.connection),
                        )
                    }
                    GroupsStep::InsertChildGroup { id, parent_id, name } => {
                        Outcome::of(
                            &diesel::insert_into(groups::table)
                                .values((
                                    groups::id.eq(*id),
                                    groups::parent_group_id.eq(Some(*parent_id)),
                                    groups::name.eq(*name),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    GroupsStep::InsertUser { id, name } => {
                        Outcome::of(
                            &diesel::insert_into(users::table)
                                .values((users::id.eq(*id), users::name.eq(*name)))
                                .execute(&mut self.connection),
                        )
                    }
                    GroupsStep::AddToGroup { group_id, user_id } => {
                        Outcome::of(
                            &diesel::insert_into(group_memberships::table)
                                .values((
                                    group_memberships::group_id.eq(*group_id),
                                    group_memberships::user_id.eq(*user_id),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    GroupsStep::RemoveFromGroup { group_id, user_id } => {
                        Outcome::of(
                            &diesel::delete(
                                group_memberships::table
                                    .filter(group_memberships::group_id.eq(*group_id))
                                    .filter(group_memberships::user_id.eq(*user_id)),
                            )
                            .execute(&mut self.connection),
                        )
                    }
                    GroupsStep::CountMemberships => {
                        let count = group_memberships::table
                            .count()
                            .get_result::<i64>(&mut self.connection)
                            .expect("count memberships");
                        Outcome::Count(count)
                    }
                }
            }
        }
    };
}

groups_runner!(GroupsPgRunner, PgConnection);
groups_runner!(GroupsSqliteRunner, SqliteConnection);

/// Adding alice to the child group propagates up to parent and grandparent,
/// giving three memberships. Removing from the parent cascades down and removes
/// the child membership, leaving one (grandparent only). Both recursive
/// directions are driven and compared.
fn groups_scenario() -> Vec<GroupsStep> {
    vec![
        GroupsStep::InsertRootGroup { id: grandparent_group(), name: "grandparent" },
        GroupsStep::InsertChildGroup {
            id: parent_group(),
            parent_id: grandparent_group(),
            name: "parent",
        },
        GroupsStep::InsertChildGroup {
            id: child_group(),
            parent_id: parent_group(),
            name: "child",
        },
        GroupsStep::InsertUser { id: alice(), name: "alice" },
        // Adding alice to child propagates to parent and grandparent.
        GroupsStep::AddToGroup { group_id: child_group(), user_id: alice() },
        GroupsStep::CountMemberships,
        // Removing alice from parent cascades to child.
        GroupsStep::RemoveFromGroup { group_id: parent_group(), user_id: alice() },
        GroupsStep::CountMemberships,
    ]
}

/// The groups fixture exercises recursive membership propagation in both
/// directions. Adding a user to a child group propagates up, removing from a
/// parent cascades down. The count after each operation is compared across
/// engines to prove the triggers fired and had the same effect.
///
/// Proof of detectability: a mutation that forced `Count(0)` for the first
/// `CountMemberships` step failed with "MUTATION PROBE groups CountMemberships
/// left: Count(3) right: Count(0)". Both engines agreed the count was 3, and
/// the wrong expectation was caught immediately. The mutation was reverted.
#[test]
fn groups_trigger_engines_agree() {
    let mut pg = {
        let mut conn = postgres_harness::fresh_database();
        postgres_harness::apply(&mut conn, GROUPS).expect("apply groups to pg");
        GroupsPgRunner { connection: conn }
    };
    let mut sq = GroupsSqliteRunner { connection: apply_to_sqlite(GROUPS) };

    for step in groups_scenario() {
        let expected = pg.step(&step);
        let actual = sq.step(&step);
        assert_eq!(actual, expected, "groups disagrees on {step:?}");
    }
}
