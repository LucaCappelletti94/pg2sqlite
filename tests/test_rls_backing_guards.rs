//! Backing-table guards: BEFORE INSERT (R2-1) and BEFORE UPDATE (R2-2), plus
//! monitor demotion from abort to log (R2-3).
//!
//! R2-1: With RLS enabled and zero INSERT policies,
//! `generate_insert_check_trigger_sql` returns `Ok(None)`, so no backing-table
//! BEFORE INSERT guard exists. In strict mode, `rewrite_rls_view_insert`
//! redirects INSERT...RETURNING straight at the backing table; without a guard
//! the row lands even though PostgreSQL would refuse (deny-by-default). Fix:
//! emit an unconditional RAISE guard whenever no INSERT or ALL policy exists,
//! and emit that guard regardless of strict mode.
//!
//! R2-2: The backing table has a BEFORE INSERT guard but no BEFORE UPDATE
//! guard, so an ON CONFLICT DO UPDATE redirected to the backing table writes
//! past UPDATE policies. Measured: bob's row overwritten by alice even though
//! UPDATE USING (owner = current_user) would block it. Fix: emit a BEFORE
//! UPDATE guard that raises when USING(OLD) IS NOT TRUE or WITH CHECK(NEW) IS
//! NOT TRUE.
//!
//! R2-3: The AFTER INSERT/UPDATE monitoring trigger aborts in strict mode
//! whenever the new row is not SELECT-visible, but PostgreSQL only requires
//! visibility when the statement carries RETURNING. Fix: remove the RAISE leg
//! from the monitoring triggers; both modes now log only, and enforcement stays
//! in the INSTEAD OF and backing-guard triggers.

mod helpers;

use diesel::prelude::*;
use helpers::{establish_connection, set_session_username};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, TranslationOptions};

// ---------------------------------------------------------------------------
// Schema definitions used across all three findings
// ---------------------------------------------------------------------------

/// Zero INSERT/ALL policies -- deny-by-default on the INSERT path.
const ZERO_POLICY_SCHEMA: &str = "
    CREATE TABLE posts (
        id INTEGER PRIMARY KEY,
        body TEXT NOT NULL DEFAULT ''
    );
    ALTER TABLE posts ENABLE ROW LEVEL SECURITY;
";

/// sel USING(true), ins WITH CHECK(true), upd FOR UPDATE USING(owner =
/// current_user). WITH CHECK(true) makes INSERT always pass; UPDATE is
/// owner-scoped.
const GUARDED_SCHEMA: &str = "
    CREATE TABLE items (
        id INTEGER PRIMARY KEY,
        owner TEXT NOT NULL,
        body TEXT NOT NULL DEFAULT ''
    );
    ALTER TABLE items ENABLE ROW LEVEL SECURITY;
    CREATE POLICY items_sel ON items FOR SELECT USING (true);
    CREATE POLICY items_ins ON items FOR INSERT WITH CHECK (true);
    CREATE POLICY items_upd ON items FOR UPDATE USING (owner = current_user);
";

/// ins WITH CHECK(true), sel USING(owner = current_user) -- the R2-3 case:
/// an insert can land without being SELECT-visible to the inserting user.
const INVISIBLE_INSERT_SCHEMA: &str = "
    CREATE TABLE events (
        id INTEGER PRIMARY KEY,
        owner TEXT NOT NULL,
        msg TEXT NOT NULL DEFAULT ''
    );
    ALTER TABLE events ENABLE ROW LEVEL SECURITY;
    CREATE POLICY events_ins ON events FOR INSERT WITH CHECK (true);
    CREATE POLICY events_sel ON events FOR SELECT USING (owner = current_user);
";

// ---------------------------------------------------------------------------
// Diesel table! schemas for typed reads/writes
// ---------------------------------------------------------------------------

mod schema {
    diesel::table! {
        posts_rls (id) {
            id -> Integer,
            body -> Text,
        }
    }
    diesel::table! {
        items_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        /// Also readable as the RLS view when SELECT USING (true).
        items (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        events_rls (id) {
            id -> Integer,
            owner -> Text,
            msg -> Text,
        }
    }
}

// ---------------------------------------------------------------------------
// Insertable structs (typed seed path)
// ---------------------------------------------------------------------------

#[derive(Insertable)]
#[diesel(table_name = schema::items_rls)]
struct ItemRow {
    id: i32,
    owner: String,
    body: String,
}

/// Minimal audit record: only the columns the tests actually assert on.
/// Using QueryableByName with an explicit SELECT keeps unused-field warnings
/// away without broad allows; diesel needs no column-complete struct here.
#[derive(QueryableByName, Debug)]
struct AuditLog {
    #[diesel(sql_type = diesel::sql_types::Text)]
    violation_type: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    details: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn strict_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_strict_rls_validation()
        .with_session_variable(SessionVariableMapping::current_user("current_app_username"))
}

fn monitor_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::current_user("current_app_username"))
}

/// Applies translated DDL to a fresh in-memory connection. The emitted SQL is
/// the artifact under test; every subsequent data-path call uses the typed DSL.
fn apply(pg: &str, opts: &Pg2SqliteOptions) -> SqliteConnection {
    let translated =
        Pg2Sqlite::default().sql(pg).expect("parse").translate(opts).expect("translate");
    let mut conn = establish_connection();
    for statement in &translated {
        // DDL emitted by the translator cannot be expressed with the typed DSL.
        diesel::sql_query(statement.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("emitted DDL failed: {e}\n{statement}"));
    }
    conn
}

/// Translates `pg_dml` in the context of `schema` and returns the tail
/// statements (those beyond the schema-only translation) as rendered strings.
fn translate_dml_tail(pg_dml: &str, schema: &str, opts: &Pg2SqliteOptions) -> Vec<String> {
    let schema_len = Pg2Sqlite::default()
        .sql(schema)
        .expect("parse schema")
        .translate(opts)
        .expect("translate schema")
        .len();
    let combined = format!("{schema}\n{pg_dml}");
    Pg2Sqlite::default()
        .sql(&combined)
        .expect("parse combined")
        .translate(opts)
        .expect("translate combined")
        .into_iter()
        .skip(schema_len)
        .map(|s| s.to_string())
        .collect()
}

/// Runs all translated DML statements; returns the last execute result.
fn run_dml(
    conn: &mut SqliteConnection,
    pg_dml: &str,
    schema: &str,
    opts: &Pg2SqliteOptions,
) -> QueryResult<usize> {
    let stmts = translate_dml_tail(pg_dml, schema, opts);
    let mut last = 0usize;
    for stmt in stmts {
        // The translator's output cannot be expressed via the typed DSL.
        last = diesel::sql_query(stmt).execute(conn)?;
    }
    Ok(last)
}

// ---------------------------------------------------------------------------
// R2-1: zero-policy deny-by-default for INSERT...RETURNING redirect
// ---------------------------------------------------------------------------

/// Before the fix: zero-policy RLS emits no backing-table BEFORE INSERT guard,
/// so a RETURNING-redirected INSERT in strict mode lands without enforcement.
/// PostgreSQL denies by default when no INSERT policy exists.
///
/// After the fix: `generate_insert_check_trigger_sql` emits an unconditional
/// RAISE guard, and that guard is emitted regardless of strict/monitor mode.
#[test]
fn zero_policy_strict_insert_returning_is_refused() {
    // No session variable needed: the schema has no policies at all.
    let opts = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_strict_rls_validation();
    let mut conn = apply(ZERO_POLICY_SCHEMA, &opts);

    // INSERT...RETURNING is redirected to posts_rls in strict mode.
    // The unconditional BEFORE INSERT guard must fire and raise.
    let result = run_dml(
        &mut conn,
        "INSERT INTO posts (body) VALUES ('hello') RETURNING id",
        ZERO_POLICY_SCHEMA,
        &opts,
    );
    assert!(result.is_err(), "zero-policy RLS must deny INSERT; got success");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("row-level security") || msg.contains("permission denied"),
        "expected policy refusal, got: {msg}"
    );
}

/// Same denial must hold in monitor mode (not only strict mode), because the
/// backing INSERT guard is now a schema artifact emitted whenever RLS is
/// active.
#[test]
fn zero_policy_monitor_insert_to_backing_table_is_refused() {
    let opts = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let mut conn = apply(ZERO_POLICY_SCHEMA, &opts);

    // Direct insert to the backing table simulates a raw write that bypasses the
    // view; this is exactly what the RETURNING redirect emits in strict mode.
    let result = diesel::insert_into(schema::posts_rls::table)
        .values((schema::posts_rls::body.eq("hello"),))
        .execute(&mut conn);
    assert!(result.is_err(), "zero-policy RLS must deny backing-table INSERT; got success");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("row-level security") || msg.contains("permission denied"),
        "expected policy refusal, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// R2-2: ON CONFLICT DO UPDATE bypasses UPDATE policies on the backing table
// ---------------------------------------------------------------------------

/// Before the fix: no BEFORE UPDATE guard exists on the backing table, so
/// ON CONFLICT DO UPDATE redirected there can overwrite a row whose owner
/// fails UPDATE USING. Measured: alice overwrites bob's row.
///
/// After the fix: a BEFORE UPDATE guard raises when USING(OLD) IS NOT TRUE.
#[test]
fn on_conflict_do_update_on_foreign_row_is_refused() {
    let mut conn = apply(GUARDED_SCHEMA, &strict_opts());
    // Set session user to alice.
    set_session_username("alice");

    // Seed bob's row directly into the backing table (bypasses INSERT trigger
    // during fixture setup; WITH CHECK(true) would also pass through the view).
    diesel::insert_into(schema::items_rls::table)
        .values(ItemRow { id: 1, owner: "bob".to_owned(), body: "original".to_owned() })
        .execute(&mut conn)
        .expect("seed bob row");

    // alice tries to upsert onto bob's id. UPDATE USING (owner = current_user)
    // evaluates against OLD.owner='bob' != 'alice' -> should raise.
    let result = run_dml(
        &mut conn,
        "INSERT INTO items (id, owner, body) VALUES (1, 'alice', 'hijack') \
         ON CONFLICT (id) DO UPDATE SET body = excluded.body",
        GUARDED_SCHEMA,
        &strict_opts(),
    );
    assert!(result.is_err(), "UPDATE USING must block alice updating bob's row; got success");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("row-level security") || msg.contains("abort"),
        "expected policy refusal, got: {msg}"
    );

    // bob's row must be unchanged.
    let body: String = schema::items_rls::table
        .filter(schema::items_rls::id.eq(1))
        .select(schema::items_rls::body)
        .first(&mut conn)
        .expect("bob row must still exist");
    assert_eq!(body, "original", "bob's row must not be overwritten");
}

/// Companion: view-path UPDATE of an owned row must still succeed.
/// The view INSTEAD OF UPDATE trigger filters by USING before forwarding to the
/// backing table; the backing BEFORE UPDATE guard sees USING(OLD)=TRUE and must
/// not raise.
#[test]
fn view_path_update_of_owned_row_succeeds() {
    let mut conn = apply(GUARDED_SCHEMA, &strict_opts());
    set_session_username("alice");

    // Seed alice's row.
    diesel::insert_into(schema::items_rls::table)
        .values(ItemRow { id: 2, owner: "alice".to_owned(), body: "old".to_owned() })
        .execute(&mut conn)
        .expect("seed alice row");

    // UPDATE through the view as alice -- USING passes.
    run_dml(
        &mut conn,
        "UPDATE items SET body = 'new' WHERE id = 2",
        GUARDED_SCHEMA,
        &strict_opts(),
    )
    .expect("view-path update of own row must succeed");

    let body: String = schema::items_rls::table
        .filter(schema::items_rls::id.eq(2))
        .select(schema::items_rls::body)
        .first(&mut conn)
        .expect("alice row must still exist");
    assert_eq!(body, "new", "alice's row must be updated");
}

// ---------------------------------------------------------------------------
// R2-3: strict monitor aborts a PostgreSQL-valid invisible insert
// ---------------------------------------------------------------------------

/// Before the fix: strict mode's AFTER INSERT monitor aborts whenever the new
/// row is not SELECT-visible, even when PostgreSQL would accept the write.
/// PostgreSQL ground truth: INSERT with no RETURNING is allowed even if the
/// new row fails the SELECT policy. Today this aborts.
///
/// After the fix: the monitor only logs; enforcement stays in the INSTEAD OF
/// and backing-guard triggers.
#[test]
fn strict_monitor_allows_invisible_view_path_insert() {
    let mut conn = apply(INVISIBLE_INSERT_SCHEMA, &strict_opts());
    set_session_username("alice");

    // INSERT bob-owned row through the view as alice, no RETURNING.
    // WITH CHECK(true) -> passes the INSERT trigger.
    // SELECT USING(owner = current_user = 'alice') -> bob's row not visible.
    // Strict monitor (old behavior): aborts because not visible.
    // After fix: logs only, insert succeeds.
    run_dml(
        &mut conn,
        "INSERT INTO events (id, owner, msg) VALUES (1, 'bob', 'hello')",
        INVISIBLE_INSERT_SCHEMA,
        &strict_opts(),
    )
    .expect("INSERT of invisible row through view must succeed without RETURNING");

    // Row is in the backing table.
    let count: i64 = schema::events_rls::table.count().get_result(&mut conn).expect("count");
    assert_eq!(count, 1, "bob's row must be in the backing table");

    // Row is not visible through alice's SELECT view.
    let visible: i64 = diesel::sql_query("SELECT COUNT(*) as count FROM events")
        .get_result::<helpers::Count>(&mut conn)
        .expect("view count")
        .count;
    assert_eq!(visible, 0, "bob's row must not be visible through alice's view");

    // Audit table has a log row with honest wording (no policy-violation claim).
    let audit: Vec<AuditLog> = diesel::sql_query("SELECT violation_type, details FROM rls_audit")
        .load(&mut conn)
        .expect("audit rows");
    assert_eq!(audit.len(), 1, "monitor must log exactly one row");
    let row = &audit[0];
    assert_ne!(
        row.violation_type, "rls_policy_violation",
        "log must not claim a policy violation for a PostgreSQL-valid write"
    );
    let details = row.details.as_deref().unwrap_or("");
    assert!(
        details.contains("RETURNING")
            || details.contains("readable")
            || details.contains("visible"),
        "details must explain the SELECT-only mismatch, got: {details}"
    );
}

/// Companion: monitor mode (non-strict) must also not claim a policy violation.
#[test]
fn monitor_mode_invisible_insert_is_logged_not_refused() {
    let mut conn = apply(INVISIBLE_INSERT_SCHEMA, &monitor_opts());
    set_session_username("alice");

    run_dml(
        &mut conn,
        "INSERT INTO events (id, owner, msg) VALUES (1, 'bob', 'hello')",
        INVISIBLE_INSERT_SCHEMA,
        &monitor_opts(),
    )
    .expect("INSERT must succeed in monitor mode");

    let audit: Vec<AuditLog> = diesel::sql_query("SELECT violation_type, details FROM rls_audit")
        .load(&mut conn)
        .expect("audit rows");
    assert_eq!(audit.len(), 1, "monitor must log one row");
    assert_ne!(
        audit[0].violation_type, "rls_policy_violation",
        "log must not claim a policy violation"
    );
}
