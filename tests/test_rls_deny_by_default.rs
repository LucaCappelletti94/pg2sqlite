//! Write-path policy enforcement, matching PostgreSQL per operation.
//!
//! PostgreSQL is deny-by-default once a table has any policy, but what "denied"
//! means differs by operation, and the difference is measured against
//! PostgreSQL 16 rather than assumed:
//!
//! - `INSERT` is checked against `WITH CHECK`. With no applicable policy
//!   nothing satisfies it, so the statement errors with "new row violates
//!   row-level security policy".
//! - `UPDATE` and `DELETE` use `USING` to choose which rows they may target.
//!   With no applicable policy no row qualifies, so they report zero rows
//!   affected and succeed.
//! - An `UPDATE` whose new row fails `WITH CHECK` errors, even though the same
//!   statement against an invisible row would have been a silent no-op.
//!
//! This file originally pinned a raise for all three, which was stricter than
//! PostgreSQL on the `UPDATE` and `DELETE` paths. That raise is still available
//! through `with_strict_rls_write_deny` for callers who want a missing policy
//! to be loud, and `strict_write_deny_raises_for_update_and_delete` covers it.
//!
//! The schema calls `current_setting('app.user_id')`, which translates to a
//! SQLite function that has to be registered on the connection before the
//! emitted script applies.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};

diesel::define_sql_function! {
    /// Resolves the session user the RLS policies compare rows against.
    fn current_app_user() -> diesel::sql_types::BigInt;
}

diesel::table! {
    /// The policy-wrapped view callers write through.
    documents (id) {
        /// Primary key.
        id -> Integer,
        /// Owning user.
        owner_id -> Integer,
        /// Document title.
        title -> Text,
    }
}

diesel::table! {
    /// The backing table the wrapper's triggers write to.
    documents_rls (id) {
        /// Primary key.
        id -> Integer,
        /// Owning user.
        owner_id -> Integer,
        /// Document title.
        title -> Text,
    }
}

diesel::table! {
    /// Target of the `ALTER TABLE ... ADD COLUMN` case.
    notes (id) {
        /// Primary key.
        id -> Integer,
        /// Owning user.
        owner_id -> Integer,
        /// Column added after the policies were declared.
        note -> Nullable<Text>,
    }
}

fn rls_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_rls_audit_table_name("rls_violations")
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
}

const SCHEMA_SELECT_INSERT_ONLY: &str = "\
CREATE TABLE documents (
    id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    title TEXT NOT NULL
);
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY documents_select_policy ON documents
    FOR SELECT
    USING (owner_id = current_setting('app.user_id')::integer);
CREATE POLICY documents_insert_policy ON documents
    FOR INSERT
    WITH CHECK (owner_id = current_setting('app.user_id')::integer);
INSERT INTO documents (id, owner_id, title) VALUES (1, 42, 'First'), (2, 42, 'Second');
";

fn open_with_session_user() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    current_app_user_utils::register_impl(&mut conn, || 42i64).expect("register current_app_user");
    conn
}

/// Applies the emitted script, which is DDL the typed DSL cannot express.
fn apply(schema: &str, opts: &Pg2SqliteOptions) -> SqliteConnection {
    let mut conn = open_with_session_user();
    conn.batch_execute(&translate(schema, opts)).expect("apply schema");
    conn
}

fn translate(schema: &str, opts: &Pg2SqliteOptions) -> String {
    Pg2Sqlite::default()
        .sql(schema)
        .expect("parse")
        .translate(opts)
        .expect("translate")
        .iter()
        .map(|s| format!("{s};"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A `DELETE` with no applicable policy affects zero rows and does not raise.
///
/// Verified against PostgreSQL 16: with row level security enabled and no
/// applicable policy, `DELETE` reports `DELETE 0` and succeeds. PostgreSQL uses
/// a policy's `USING` clause to decide which rows the statement can target, so
/// when none qualify there is simply nothing to delete. Raising here diverged
/// from that, and is now available behind `with_strict_rls_write_deny`.
#[test]
fn delete_without_a_policy_affects_no_rows_and_does_not_raise() {
    let mut conn = apply(SCHEMA_SELECT_INSERT_ONLY, &rls_opts());

    let before: i64 = documents::table.count().get_result(&mut conn).expect("count before");
    assert_eq!(before, 2);
    let result = diesel::delete(documents::table.find(1)).execute(&mut conn);
    let after: i64 = documents::table.count().get_result(&mut conn).expect("count after");

    assert!(result.is_ok(), "DELETE must succeed as it does in PostgreSQL, got {result:?}");
    assert_eq!(
        after, before,
        "no row may be removed without a DELETE policy; row count changed {before} -> {after}"
    );
}

/// An `UPDATE` with no applicable policy affects zero rows and does not raise.
///
/// Same PostgreSQL 16 measurement as the DELETE case above: `UPDATE 0`, no
/// error.
#[test]
fn update_without_a_policy_changes_nothing_and_does_not_raise() {
    let mut conn = apply(SCHEMA_SELECT_INSERT_ONLY, &rls_opts());

    let result = diesel::update(documents::table.find(1))
        .set(documents::title.eq("Hijacked"))
        .execute(&mut conn);
    let title: String = documents::table
        .find(1)
        .select(documents::title)
        .first(&mut conn)
        .expect("the row stays readable");

    assert!(result.is_ok(), "UPDATE must succeed as it does in PostgreSQL, got {result:?}");
    assert_ne!(title, "Hijacked", "the row must not change without an UPDATE policy");
}

/// The strict opt-in restores the raise for both operations.
#[test]
fn strict_write_deny_raises_for_update_and_delete() {
    let mut conn = apply(SCHEMA_SELECT_INSERT_ONLY, &rls_opts().with_strict_rls_write_deny());

    assert!(
        diesel::update(documents::table.find(1))
            .set(documents::title.eq("Hijacked"))
            .execute(&mut conn)
            .is_err(),
        "strict write deny must raise on an UPDATE with no policy"
    );
    assert!(
        diesel::delete(documents::table.find(1)).execute(&mut conn).is_err(),
        "strict write deny must raise on a DELETE with no policy"
    );

    let after: i64 = documents::table.count().get_result(&mut conn).expect("count after");
    assert_eq!(after, 2, "neither statement may change data");
}

const SCHEMA_FULL_POLICIES: &str = "\
CREATE TABLE documents (
    id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    title TEXT NOT NULL
);
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY documents_select_policy ON documents
    FOR SELECT USING (owner_id = current_setting('app.user_id')::integer);
CREATE POLICY documents_insert_policy ON documents
    FOR INSERT WITH CHECK (owner_id = current_setting('app.user_id')::integer);
CREATE POLICY documents_update_policy ON documents
    FOR UPDATE USING (owner_id = current_setting('app.user_id')::integer);
CREATE POLICY documents_delete_policy ON documents
    FOR DELETE USING (owner_id = current_setting('app.user_id')::integer);
INSERT INTO documents (id, owner_id, title) VALUES (1, 42, 'First'), (2, 42, 'Second');
";

#[test]
fn delete_with_matching_policy_succeeds() {
    // Positive sanity check: once a FOR DELETE policy IS declared,
    // and the current row passes its USING predicate, DELETE must
    // succeed. This test must already pass today and must keep
    // passing after the deny-by-default fix lands.
    let mut conn = apply(SCHEMA_FULL_POLICIES, &rls_opts());

    let result = diesel::delete(documents::table.find(1)).execute(&mut conn);
    assert!(result.is_ok(), "DELETE with a matching FOR DELETE policy must succeed: {result:?}");
    let remaining: i64 = documents::table.count().get_result(&mut conn).expect("count remaining");
    assert_eq!(remaining, 1, "exactly one row should have been deleted");
}

#[test]
fn update_with_matching_policy_succeeds() {
    let mut conn = apply(SCHEMA_FULL_POLICIES, &rls_opts());

    let result = diesel::update(documents::table.find(1))
        .set(documents::title.eq("Updated"))
        .execute(&mut conn);
    assert!(result.is_ok(), "UPDATE with a matching FOR UPDATE policy must succeed: {result:?}");
    let title: String = documents::table
        .find(1)
        .select(documents::title)
        .first(&mut conn)
        .expect("the updated row stays readable");
    assert_eq!(title, "Updated");
}

const SCHEMA_SELECT_ONLY: &str = "\
CREATE TABLE documents (
    id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    title TEXT NOT NULL
);
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY documents_select_policy ON documents
    FOR SELECT
    USING (owner_id = current_setting('app.user_id')::integer);
";

#[test]
fn insert_is_blocked_when_no_insert_policy_is_defined() {
    // Symmetric coverage for INSERT: with a FOR SELECT policy only, every
    // INSERT must raise. Seeded rows are intentionally absent here so we
    // exercise the post-apply INSERT path directly.
    let mut conn = apply(SCHEMA_SELECT_ONLY, &rls_opts());

    let result = diesel::insert_into(documents::table)
        .values((documents::id.eq(1), documents::owner_id.eq(42), documents::title.eq("mine")))
        .execute(&mut conn);
    let count: i64 = documents::table.count().get_result(&mut conn).expect("count after");
    assert!(
        result.is_err(),
        "INSERT must be rejected when no FOR INSERT policy is declared; got Ok ({result:?})"
    );
    assert_eq!(count, 0, "no row may be written when INSERT is denied");
}

/// A table whose only policy is `FOR SELECT`: the `INSERT` is rejected while
/// the `UPDATE` and `DELETE` quietly affect nothing, which is what PostgreSQL
/// does.
///
/// The asymmetry is the point and it is measured, not assumed. PostgreSQL
/// checks `WITH CHECK` for an inserted row, and with no `FOR INSERT` policy
/// nothing can satisfy it, so the insert errors. For `UPDATE` and `DELETE` it
/// instead uses `USING` to choose targetable rows, so no applicable policy
/// means no candidate rows and a zero count.
///
/// A row is seeded straight into the backing table so the INSTEAD OF triggers
/// really fire: under SQLite's FOR EACH ROW semantics an UPDATE matching no
/// view row never invokes its trigger, which would make this pass for the wrong
/// reason.
#[test]
fn select_only_table_blocks_inserts_and_silently_ignores_other_writes() {
    let mut conn = apply(SCHEMA_SELECT_ONLY, &rls_opts());

    // Seed via backing table simulating system-level data loading that runs
    // with triggers disabled (authoritative-apply pattern). The BEFORE INSERT
    // guard on a zero-policy table is unconditional; disable it for the seed.
    conn.set_triggers_enabled(false).expect("disable triggers for seed");
    diesel::insert_into(documents_rls::table)
        .values((
            documents_rls::id.eq(1),
            documents_rls::owner_id.eq(42),
            documents_rls::title.eq("seed"),
        ))
        .execute(&mut conn)
        .expect("seed via backing table");
    conn.set_triggers_enabled(true).expect("re-enable triggers");

    let insert_err = diesel::insert_into(documents::table)
        .values((documents::id.eq(2), documents::owner_id.eq(42), documents::title.eq("t")))
        .execute(&mut conn)
        .expect_err("INSERT must be rejected");
    assert!(
        insert_err.to_string().contains("INSERT") && insert_err.to_string().contains("policy"),
        "INSERT error must reference policy: {insert_err:?}"
    );

    assert!(
        diesel::update(documents::table.find(1))
            .set(documents::title.eq("t"))
            .execute(&mut conn)
            .is_ok(),
        "UPDATE must succeed, affecting nothing"
    );
    assert!(
        diesel::delete(documents::table.find(1)).execute(&mut conn).is_ok(),
        "DELETE must succeed, affecting nothing"
    );

    let title: String = documents_rls::table
        .find(1)
        .select(documents_rls::title)
        .first(&mut conn)
        .expect("the backing row stays readable");
    assert_eq!(title, "seed", "the backing row must be untouched by either write");
    let count: i64 = documents_rls::table.count().get_result(&mut conn).expect("backing count");
    assert_eq!(count, 1, "no row may be added or removed");
}

/// A policy must survive a column `ALTER` on its table.
///
/// The raw `CREATE TABLE` statement used to be handed to the RLS pipeline, and
/// after an `ALTER` the schema holds a modified clone that node no longer
/// matches, so `policies` answered empty while `has_row_level_security` still
/// answered true, and the wrapper degraded to deny-by-default: every INSERT
/// through the view died with `permission denied: no INSERT policy`.
///
/// The `ALTER` sits above the seed rows because the wrapper's triggers speak
/// the final column set, so DML written between the wrapper and the `ALTER`
/// would reference a column the backing table does not hold yet, the known
/// cost of translating against one schema snapshot.
#[test]
fn a_policy_survives_an_alter_add_column_on_its_table() {
    let mut conn = apply(
        "CREATE TABLE notes (
             id INTEGER PRIMARY KEY,
             owner_id INTEGER NOT NULL
         );
         ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
         CREATE POLICY notes_select ON notes
             FOR SELECT
             USING (owner_id = current_setting('app.user_id')::integer);
         CREATE POLICY notes_insert ON notes
             FOR INSERT
             WITH CHECK (owner_id = current_setting('app.user_id')::integer);
         ALTER TABLE notes ADD COLUMN note TEXT;
         INSERT INTO notes (id, owner_id, note) VALUES (3, 42, 'kept');",
        &rls_opts(),
    );

    let note: Option<String> = notes::table
        .find(3)
        .select(notes::note)
        .first(&mut conn)
        .expect("the row must be visible through the view");
    assert_eq!(note.as_deref(), Some("kept"));
}
