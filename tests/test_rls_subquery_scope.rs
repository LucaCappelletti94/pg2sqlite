//! Pins the subquery qualifier-rewrite path in the RLS expression transformer.
//!
//! A policy with a correlated EXISTS subquery references two tables: the
//! guarded table (whose column refs become `NEW.col` or `OLD.col` in trigger
//! guards) and the subquery's own FROM table. Only the guarded table's
//! qualified refs must receive the prefix. A qualifier naming any other table
//! must keep its own name.

mod helpers;
use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};
// Tests (a) and (c): members + t, where members.team is absent from t.
diesel::table! {
    /// Lookup table used in the correlated-subquery policy tests.
    members (owner) {
        /// Membership owner identifier.
        owner -> Text,
        /// Team the owner belongs to.
        team -> Text,
    }
}

diesel::table! {
    /// Backing table for the t view used in tests (a) and (c).
    t_rls (id) {
        /// Row id.
        id -> Integer,
        /// Owner of the row.
        owner -> Text,
        /// Arbitrary integer payload.
        n -> Integer,
    }
}

diesel::table! {
    /// View for t with a correlated-subquery RLS policy (tests a and c).
    t (id) {
        /// Row id.
        id -> Integer,
        /// Owner of the row.
        owner -> Text,
        /// Arbitrary integer payload.
        n -> Integer,
    }
}

// Test (b): members2 + t2, where owner is a column of both tables.
diesel::table! {
    /// Members table for the shared-column-name tautology test (b).
    members2 (owner) {
        /// Member owner identifier.
        owner -> Text,
    }
}

diesel::table! {
    /// Backing table for the t2 view used in test (b).
    t2_rls (id) {
        /// Row id.
        id -> Integer,
        /// Owner of the row.
        owner -> Text,
        /// Arbitrary integer payload.
        n -> Integer,
    }
}

diesel::table! {
    /// View for t2 with a correlated-subquery RLS policy (test b).
    t2 (id) {
        /// Row id.
        id -> Integer,
        /// Owner of the row.
        owner -> Text,
        /// Arbitrary integer payload.
        n -> Integer,
    }
}

// Test (d): access_list (RLS) + resources (RLS policy reads access_list).
diesel::table! {
    /// Backing table for the access_list view used in test (d).
    access_list_rls (user_id) {
        /// User identifier.
        user_id -> Text,
    }
}

diesel::table! {
    /// View for access_list with a session-user RLS policy (test d).
    access_list (user_id) {
        /// User identifier.
        user_id -> Text,
    }
}

diesel::table! {
    /// Backing table for the resources view used in test (d).
    resources_rls (id) {
        /// Resource id.
        id -> Integer,
        /// Owner of the resource.
        owner -> Text,
    }
}

diesel::table! {
    /// View for resources whose RLS policy reads access_list (test d).
    resources (id) {
        /// Resource id.
        id -> Integer,
        /// Owner of the resource.
        owner -> Text,
    }
}

const POLICY_VIOLATION: &str = "new row violates row-level security policy";

fn base_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit")
}

fn opts_with_username() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit").with_session_variable(
        SessionVariableMapping::current_setting("app.username", "current_app_username"),
    )
}

/// Translates `pg_sql` and applies every emitted statement to a fresh
/// in-memory connection. The emitted DDL is the artifact under test, so it
/// runs as generated text. All data operations use the typed DSL.
fn apply(pg_sql: &str, opts: &Pg2SqliteOptions) -> SqliteConnection {
    let stmts =
        Pg2Sqlite::default().sql(pg_sql).expect("parse").translate(opts).expect("translate");
    let mut conn = helpers::establish_connection();
    for stmt in &stmts {
        // Raw SQL is justified: the emitted DDL is the artifact under test.
        diesel::sql_query(stmt.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("emitted DDL failed: {e}\n{stmt}"));
    }
    conn
}

/// Unaliased correlated subquery where the referenced table has a column that
/// the guarded table does not. The prefix bug rewrites `members.team` to
/// `NEW.team`, which SQLite rejects as "no such column" when the trigger fires.
/// After the fix, `members.owner = NEW.owner` is the correct correlation and
/// `members.team = 'blue'` is a table-scoped comparison, so admitted rows land
/// and forbidden rows are refused.
#[test]
fn unaliased_cross_table_distinct_column() {
    const SQL: &str = r#"
        CREATE TABLE members (owner TEXT PRIMARY KEY, team TEXT NOT NULL);
        CREATE TABLE t (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, n INTEGER);
        ALTER TABLE t ENABLE ROW LEVEL SECURITY;
        CREATE POLICY p ON t USING (
            EXISTS (SELECT 1 FROM members WHERE members.owner = t.owner AND members.team = 'blue')
        );
    "#;
    let mut conn = apply(SQL, &base_opts());

    // alice is in the blue team, bob in the red team.
    diesel::insert_into(members::table)
        .values(&[
            (members::owner.eq("alice"), members::team.eq("blue")),
            (members::owner.eq("bob"), members::team.eq("red")),
        ])
        .execute(&mut conn)
        .expect("seed members");

    // alice is in the blue team so the policy admits her row.
    diesel::insert_into(t::table)
        .values((t::id.eq(1), t::owner.eq("alice"), t::n.eq(0)))
        .execute(&mut conn)
        .expect("admitted row must land");

    let stored: Vec<(i32, String, i32)> = t_rls::table
        .select((t_rls::id, t_rls::owner, t_rls::n))
        .load(&mut conn)
        .expect("read backing table");
    assert_eq!(
        stored,
        vec![(1, "alice".to_owned(), 0)],
        "admitted row must be visible in the backing table"
    );

    // bob is not in the blue team so the policy forbids his row.
    let err = diesel::insert_into(t::table)
        .values((t::id.eq(2), t::owner.eq("bob"), t::n.eq(0)))
        .execute(&mut conn)
        .expect_err("forbidden row must be refused");
    assert!(err.to_string().contains(POLICY_VIOLATION), "expected policy violation but got: {err}");
}

/// The same policy shape but `owner` is a column of both the guarded table and
/// the subquery's FROM table. The prefix bug turned `members2.owner = t2.owner`
/// into the tautology `NEW.owner = NEW.owner`, admitting every row as long as
/// any member exists. After the fix the correlation is
/// `members2.owner = NEW.owner` and the guard is enforced correctly.
#[test]
fn unaliased_cross_table_shared_column_name() {
    const SQL: &str = r#"
        CREATE TABLE members2 (owner TEXT PRIMARY KEY);
        CREATE TABLE t2 (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, n INTEGER);
        ALTER TABLE t2 ENABLE ROW LEVEL SECURITY;
        CREATE POLICY p ON t2 USING (
            EXISTS (SELECT 1 FROM members2 WHERE members2.owner = t2.owner)
        );
    "#;
    let mut conn = apply(SQL, &base_opts());

    // Seed alice as the only member.
    diesel::insert_into(members2::table)
        .values(members2::owner.eq("alice"))
        .execute(&mut conn)
        .expect("seed members2");

    // alice is in members2 so her row must land.
    diesel::insert_into(t2::table)
        .values((t2::id.eq(1), t2::owner.eq("alice"), t2::n.eq(0)))
        .execute(&mut conn)
        .expect("admitted row must land");

    // charlie is not in members2 so his row must be refused, not slip through
    // the tautology that the prefix bug creates.
    let err = diesel::insert_into(t2::table)
        .values((t2::id.eq(2), t2::owner.eq("charlie"), t2::n.eq(0)))
        .execute(&mut conn)
        .expect_err("non-member must be refused");
    assert!(err.to_string().contains(POLICY_VIOLATION), "expected policy violation but got: {err}");

    let count: i64 = t2_rls::table.count().get_result(&mut conn).expect("count");
    assert_eq!(count, 1, "only the admitted row must be in the backing table");
}

/// An aliased correlated subquery was already correct before the fix. The alias
/// decouples the WHERE-clause qualifier from the table name, so the prefix bug
/// never fired for aliased references. This test confirms the fix does not
/// regress the aliased case.
#[test]
fn aliased_cross_table_keeps_working() {
    const SQL: &str = r#"
        CREATE TABLE members (owner TEXT PRIMARY KEY, team TEXT NOT NULL);
        CREATE TABLE t (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, n INTEGER);
        ALTER TABLE t ENABLE ROW LEVEL SECURITY;
        CREATE POLICY p ON t USING (
            EXISTS (SELECT 1 FROM members m WHERE m.owner = t.owner AND m.team = 'blue')
        );
    "#;
    let mut conn = apply(SQL, &base_opts());

    diesel::insert_into(members::table)
        .values(&[
            (members::owner.eq("alice"), members::team.eq("blue")),
            (members::owner.eq("bob"), members::team.eq("red")),
        ])
        .execute(&mut conn)
        .expect("seed");

    diesel::insert_into(t::table)
        .values((t::id.eq(1), t::owner.eq("alice"), t::n.eq(0)))
        .execute(&mut conn)
        .expect("admitted row must land");

    let err = diesel::insert_into(t::table)
        .values((t::id.eq(2), t::owner.eq("bob"), t::n.eq(0)))
        .execute(&mut conn)
        .expect_err("forbidden row must be refused");
    assert!(err.to_string().contains(POLICY_VIOLATION), "expected policy violation but got: {err}");
}

/// When the subquery's FROM table carries its own RLS SELECT policy, the
/// trigger guard must read through that table's view so the second policy
/// applies. A row the current session user cannot see in the second table must
/// be absent from the guard's perspective.
#[test]
fn policy_reads_second_rls_table_through_its_view() {
    // access_list controls whose entries are visible (each user sees only their
    // own row). resources uses access_list membership to gate inserts: a
    // resource is insertable only when the session user can see the
    // access_list entry matching resources.owner.
    const SQL: &str = r#"
        CREATE TABLE access_list (user_id TEXT PRIMARY KEY);
        ALTER TABLE access_list ENABLE ROW LEVEL SECURITY;
        CREATE POLICY access_list_p ON access_list
            USING (user_id = current_setting('app.username'));

        CREATE TABLE resources (id INTEGER PRIMARY KEY, owner TEXT NOT NULL);
        ALTER TABLE resources ENABLE ROW LEVEL SECURITY;
        CREATE POLICY resources_p ON resources
            USING (EXISTS (
                SELECT 1 FROM access_list WHERE access_list.user_id = resources.owner
            ));
    "#;

    let opts = opts_with_username();
    let mut conn = apply(SQL, &opts);

    // Set alice as the session user before any DB operation. The monitoring
    // trigger on access_list_rls evaluates the access_list view, which calls
    // current_app_username(), so the session must be established first.
    helpers::set_session_username("alice");

    // Seed alice's access_list entry directly into the backing table,
    // simulating a server-side sync that bypasses the view.
    diesel::insert_into(access_list_rls::table)
        .values(access_list_rls::user_id.eq("alice"))
        .execute(&mut conn)
        .expect("seed access_list backing table");

    // As alice (already set above), inserting a resource owned by alice must
    // succeed: alice can see her own access_list entry so EXISTS is satisfied.
    diesel::insert_into(resources::table)
        .values((resources::id.eq(1), resources::owner.eq("alice")))
        .execute(&mut conn)
        .expect("alice can insert her own resource");

    let count: i64 =
        resources_rls::table.count().get_result(&mut conn).expect("count after alice insert");
    assert_eq!(count, 1);

    // As bob, inserting a resource owned by alice must fail: bob cannot see
    // alice's access_list entry, so EXISTS finds nothing and the guard denies
    // the write. Without behavior 2 the guard reads the backing table and sees
    // alice's entry regardless of session user, letting the insert through.
    helpers::set_session_username("bob");
    let err = diesel::insert_into(resources::table)
        .values((resources::id.eq(2), resources::owner.eq("alice")))
        .execute(&mut conn)
        .expect_err("bob must not insert a resource owned by alice");
    assert!(err.to_string().contains(POLICY_VIOLATION), "expected policy violation but got: {err}");

    let count: i64 =
        resources_rls::table.count().get_result(&mut conn).expect("count after bob attempt");
    assert_eq!(count, 1, "no second row must have been inserted");
}

/// Two tables whose read policies reference each other through unaliased
/// subquery FROM clauses create a mutually-dependent pair of views. SQLite
/// silently accepts the view definitions but fails at query time with "view
/// circularly defined". The translator must refuse this at translation time and
/// name both tables in the error message.
#[test]
fn mutual_unaliased_read_policies_refused_at_translation() {
    const SQL: &str = r#"
        CREATE TABLE a (id INTEGER PRIMARY KEY, b_id INTEGER);
        ALTER TABLE a ENABLE ROW LEVEL SECURITY;
        CREATE POLICY a_p ON a
            USING (EXISTS (SELECT 1 FROM b WHERE b.id = a.b_id));

        CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER);
        ALTER TABLE b ENABLE ROW LEVEL SECURITY;
        CREATE POLICY b_p ON b
            USING (EXISTS (SELECT 1 FROM a WHERE a.id = b.a_id));
    "#;

    let result = Pg2Sqlite::default().sql(SQL).expect("parse").translate(&base_opts());

    let err = result.expect_err("mutual-reference cycle must be refused at translation");
    let msg = err.to_string();
    assert!(
        msg.contains("on a") && msg.contains("on b"),
        "error message must name both tables; got: {msg}"
    );
}
