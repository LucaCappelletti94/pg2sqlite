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

use std::cell::Cell;

mod helpers;

use diesel::prelude::*;
use helpers::{establish_connection, set_session_username};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};

diesel::define_sql_function! {
    /// Reports whether the current backing write is exempt from RLS checks.
    fn write_is_exempt() -> diesel::sql_types::Bool;
}

thread_local! {
    static WRITE_IS_EXEMPT: Cell<bool> = const { Cell::new(false) };
}

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

const SHARED_READ_OWNER_WRITE_SCHEMA: &str = "
    CREATE TABLE shared_items (
        id INTEGER PRIMARY KEY,
        owner TEXT NOT NULL,
        body TEXT NOT NULL DEFAULT ''
    );
    ALTER TABLE shared_items ENABLE ROW LEVEL SECURITY;
    CREATE POLICY shared_items_select ON shared_items FOR SELECT USING (true);
    CREATE POLICY shared_items_insert ON shared_items
        FOR INSERT WITH CHECK (owner = current_user);
    CREATE POLICY shared_items_update ON shared_items
        FOR UPDATE USING (owner = current_user) WITH CHECK (owner = current_user);
";

const NESTED_OWNERSHIP_SCHEMA: &str = "
    CREATE TABLE item_access (
        item_id INTEGER PRIMARY KEY,
        reader TEXT NOT NULL,
        writer TEXT NOT NULL
    );
    CREATE TABLE nested_items (
        id INTEGER PRIMARY KEY,
        owner TEXT NOT NULL
    );
    ALTER TABLE nested_items ENABLE ROW LEVEL SECURITY;
    CREATE POLICY nested_items_select ON nested_items FOR SELECT USING (
        EXISTS (
            SELECT 1 FROM item_access AS access
            WHERE access.item_id = nested_items.id
              AND access.reader = current_user
        )
    );
    CREATE POLICY nested_items_insert ON nested_items FOR INSERT WITH CHECK (
        EXISTS (
            SELECT 1 FROM item_access AS access
            WHERE access.item_id = nested_items.id
              AND access.writer = current_user
        )
    );
";

const CASCADED_RLS_SCHEMA: &str = "
    CREATE TABLE parent_items (
        id INTEGER PRIMARY KEY,
        owner TEXT NOT NULL
    );
    CREATE TABLE child_items (
        id INTEGER PRIMARY KEY,
        owner TEXT NOT NULL
    );
    ALTER TABLE parent_items ENABLE ROW LEVEL SECURITY;
    ALTER TABLE child_items ENABLE ROW LEVEL SECURITY;
    CREATE POLICY parent_items_select ON parent_items FOR SELECT USING (true);
    CREATE POLICY child_items_select ON child_items FOR SELECT USING (true);
    CREATE POLICY parent_items_insert ON parent_items
        FOR INSERT WITH CHECK (owner = current_user);
    CREATE POLICY child_items_insert ON child_items
        FOR INSERT WITH CHECK (owner = current_user);
    CREATE FUNCTION copy_parent_to_child() RETURNS TRIGGER AS $$
    BEGIN
        INSERT INTO child_items (id, owner) VALUES (NEW.id, NEW.owner);
        RETURN NEW;
    END;
    $$ LANGUAGE plpgsql;
    CREATE TRIGGER copy_parent_to_child
        AFTER INSERT ON parent_items
        FOR EACH ROW EXECUTE FUNCTION copy_parent_to_child();
";

const POLICY_FUNCTION_SCHEMA: &str = "
    CREATE TABLE policy_items (
        id INTEGER PRIMARY KEY,
        body TEXT NOT NULL
    );
    ALTER TABLE policy_items ENABLE ROW LEVEL SECURITY;
    CREATE POLICY policy_items_select ON policy_items FOR SELECT USING (true);
    CREATE POLICY policy_items_insert ON policy_items FOR INSERT WITH CHECK (policy_probe());
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
    diesel::table! {
        events (id) {
            id -> Integer,
            owner -> Text,
            msg -> Text,
        }
    }
    diesel::table! {
        shared_items_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        shared_items (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        item_access (item_id) {
            item_id -> Integer,
            reader -> Text,
            writer -> Text,
        }
    }
    diesel::table! {
        nested_items_rls (id) {
            id -> Integer,
            owner -> Text,
        }
    }
    diesel::table! {
        nested_items (id) {
            id -> Integer,
            owner -> Text,
        }
    }
    diesel::table! {
        parent_items_rls (id) {
            id -> Integer,
            owner -> Text,
        }
    }
    diesel::table! {
        child_items_rls (id) {
            id -> Integer,
            owner -> Text,
        }
    }
    diesel::table! {
        side_effects (id) {
            id -> Integer,
        }
    }
    diesel::table! {
        branch_items_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        branch_items (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
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

fn exemption_opts() -> Pg2SqliteOptions {
    strict_opts().with_write_exemption_function("write_is_exempt")
}

fn set_write_exempt(exempt: bool) {
    WRITE_IS_EXEMPT.with(|value| value.set(exempt));
}

fn apply_with_exemption(pg: &str) -> SqliteConnection {
    set_write_exempt(false);
    let mut conn = apply(pg, &exemption_opts());
    write_is_exempt_utils::register_nondeterministic_impl(&mut conn, || {
        WRITE_IS_EXEMPT.with(Cell::get)
    })
    .expect("register write_is_exempt");
    conn
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

fn branch_schema(policies: &str) -> String {
    format!(
        "CREATE TABLE branch_items (
             id INTEGER PRIMARY KEY,
             owner TEXT NOT NULL,
             body TEXT NOT NULL
         );
         ALTER TABLE branch_items ENABLE ROW LEVEL SECURITY;
         CREATE POLICY branch_select ON branch_items FOR SELECT USING (true);
         {policies}"
    )
}

struct UpdateGuardCase {
    name: &'static str,
    policies: &'static str,
    seed_owner: &'static str,
    update_owner: &'static str,
}

fn assert_update_guard_case(case: &UpdateGuardCase) {
    let mut conn = apply_with_exemption(&branch_schema(case.policies));
    set_write_exempt(true);
    diesel::insert_into(schema::branch_items_rls::table)
        .values((
            schema::branch_items_rls::id.eq(1),
            schema::branch_items_rls::owner.eq(case.seed_owner),
            schema::branch_items_rls::body.eq("original"),
        ))
        .execute(&mut conn)
        .unwrap_or_else(|error| panic!("{} seed failed: {error}", case.name));

    set_write_exempt(false);
    let _ = diesel::update(schema::branch_items::table.find(1))
        .set((
            schema::branch_items::owner.eq(case.update_owner),
            schema::branch_items::body.eq("blocked"),
        ))
        .execute(&mut conn);
    let stored = schema::branch_items_rls::table
        .find(1)
        .select((schema::branch_items_rls::owner, schema::branch_items_rls::body))
        .first::<(String, String)>(&mut conn)
        .unwrap();
    assert_eq!(stored, (case.seed_owner.to_owned(), "original".to_owned()), "{}", case.name);

    set_write_exempt(true);
    diesel::update(schema::branch_items::table.find(1))
        .set((
            schema::branch_items::owner.eq(case.update_owner),
            schema::branch_items::body.eq("restored"),
        ))
        .execute(&mut conn)
        .unwrap_or_else(|error| panic!("{} exempt update failed: {error}", case.name));
    let stored = schema::branch_items_rls::table
        .find(1)
        .select((schema::branch_items_rls::owner, schema::branch_items_rls::body))
        .first::<(String, String)>(&mut conn)
        .unwrap();
    assert_eq!(stored, (case.update_owner.to_owned(), "restored".to_owned()), "{}", case.name);
    set_write_exempt(false);
}

fn rendered_translation(pg: &str, opts: &Pg2SqliteOptions) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(opts)
        .expect("translate")
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn apply_rendered(conn: &rusqlite::Connection, statements: &[String]) {
    for statement in statements {
        conn.execute_batch(statement).expect("apply translated statement");
    }
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

#[test]
fn zero_policy_backing_insert_honours_write_exemption() {
    let options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_write_exemption_function("write_is_exempt");
    set_write_exempt(false);
    let mut conn = apply(ZERO_POLICY_SCHEMA, &options);
    write_is_exempt_utils::register_nondeterministic_impl(&mut conn, || {
        WRITE_IS_EXEMPT.with(Cell::get)
    })
    .expect("register write_is_exempt");

    assert!(
        diesel::insert_into(schema::posts_rls::table)
            .values(schema::posts_rls::body.eq("blocked"))
            .execute(&mut conn)
            .is_err(),
        "the default state must enforce the deny-all guard"
    );

    set_write_exempt(true);
    diesel::insert_into(schema::posts_rls::table)
        .values(schema::posts_rls::body.eq("restored"))
        .execute(&mut conn)
        .expect("the exemption must bypass the deny-all guard");
}

#[test]
fn configured_but_unregistered_exemption_fails_closed() {
    let mut conn = apply(SHARED_READ_OWNER_WRITE_SCHEMA, &exemption_opts());
    set_session_username("alice");

    let error = diesel::insert_into(schema::shared_items_rls::table)
        .values((
            schema::shared_items_rls::id.eq(1),
            schema::shared_items_rls::owner.eq("bob"),
            schema::shared_items_rls::body.eq("server"),
        ))
        .execute(&mut conn)
        .expect_err("an unavailable exemption function must not allow the write");
    assert!(
        error.to_string().contains("write_is_exempt"),
        "the missing function must be named: {error}"
    );
}

#[test]
fn innocuous_nondeterministic_exemption_works_with_trusted_schema_off() {
    use rusqlite::{Connection, functions::FunctionFlags};

    let translated = rendered_translation(ZERO_POLICY_SCHEMA, &exemption_opts());
    let conn = Connection::open_in_memory().expect("open");
    conn.create_scalar_function(
        "write_is_exempt",
        0,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
        |_| Ok(true),
    )
    .expect("register innocuous write exemption");
    conn.execute_batch("PRAGMA trusted_schema = OFF;").expect("disable trusted schema");
    apply_rendered(&conn, &translated);

    conn.execute("INSERT INTO posts_rls (body) VALUES ('restored')", [])
        .expect("hardened SQLite accepts the exemption function in a trigger");
    let count: i64 =
        conn.query_row("SELECT COUNT(*) FROM posts_rls", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn null_and_error_exemption_results_abort_writes() {
    use rusqlite::{Connection, Error, functions::FunctionFlags};

    let translated = rendered_translation(ZERO_POLICY_SCHEMA, &exemption_opts());
    let null_conn = Connection::open_in_memory().expect("open");
    null_conn
        .create_scalar_function("write_is_exempt", 0, FunctionFlags::SQLITE_UTF8, |_| {
            Ok(None::<bool>)
        })
        .expect("register nullable exemption");
    apply_rendered(&null_conn, &translated);
    assert!(null_conn.execute("INSERT INTO posts_rls (body) VALUES ('blocked')", []).is_err());

    let error_conn = Connection::open_in_memory().expect("open");
    error_conn
        .create_scalar_function(
            "write_is_exempt",
            0,
            FunctionFlags::SQLITE_UTF8,
            |_| -> rusqlite::Result<bool> {
                Err(Error::UserFunctionError(Box::new(std::io::Error::other("exemption failed"))))
            },
        )
        .expect("register failing exemption");
    apply_rendered(&error_conn, &translated);
    assert!(error_conn.execute("INSERT INTO posts_rls (body) VALUES ('blocked')", []).is_err());
}

#[test]
fn exemption_short_circuits_policy_calls_but_not_function_resolution() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use rusqlite::{Connection, functions::FunctionFlags};

    let opts = exemption_opts().with_user_defined_functions(["policy_probe"]);
    let translated = rendered_translation(POLICY_FUNCTION_SCHEMA, &opts);
    let calls = Arc::new(AtomicUsize::new(0));
    let conn = Connection::open_in_memory().expect("open");
    conn.create_scalar_function("write_is_exempt", 0, FunctionFlags::SQLITE_UTF8, |_| Ok(true))
        .expect("register exemption");
    let function_calls = Arc::clone(&calls);
    conn.create_scalar_function("policy_probe", 0, FunctionFlags::SQLITE_UTF8, move |_| {
        function_calls.fetch_add(1, Ordering::Relaxed);
        Ok(false)
    })
    .expect("register policy function");
    apply_rendered(&conn, &translated);
    conn.execute("INSERT INTO policy_items (id, body) VALUES (1, 'restored')", [])
        .expect("exemption bypasses the policy");
    assert_eq!(calls.load(Ordering::Relaxed), 0);

    let missing_conn = Connection::open_in_memory().expect("open");
    missing_conn
        .create_scalar_function("write_is_exempt", 0, FunctionFlags::SQLITE_UTF8, |_| Ok(true))
        .expect("register exemption");
    apply_rendered(&missing_conn, &translated);
    let error = missing_conn
        .execute("INSERT INTO policy_items (id, body) VALUES (1, 'blocked')", [])
        .expect_err("SQLite must resolve every trigger function");
    assert!(error.to_string().contains("policy_probe"));
}

#[test]
fn keyword_shaped_exemption_function_name_is_quoted() {
    use rusqlite::{Connection, functions::FunctionFlags};

    let opts = strict_opts().with_write_exemption_function("select");
    let translated = rendered_translation(ZERO_POLICY_SCHEMA, &opts);
    let conn = Connection::open_in_memory().expect("open");
    conn.create_scalar_function("select", 0, FunctionFlags::SQLITE_UTF8, |_| Ok(true))
        .expect("register keyword-shaped exemption");
    apply_rendered(&conn, &translated);
    conn.execute("INSERT INTO posts_rls (body) VALUES ('restored')", [])
        .expect("quoted exemption function remains callable");
}

#[test]
fn hidden_exempt_row_still_reaches_monitoring() {
    let opts = exemption_opts();
    let mut conn = apply_with_exemption(INVISIBLE_INSERT_SCHEMA);
    set_session_username("alice");
    set_write_exempt(true);
    run_dml(
        &mut conn,
        "INSERT INTO events (id, owner, msg) VALUES (1, 'bob', 'restored')",
        INVISIBLE_INSERT_SCHEMA,
        &opts,
    )
    .expect("exempt hidden insert");
    set_write_exempt(false);

    let audit = diesel::sql_query("SELECT violation_type, details FROM rls_audit")
        .load::<AuditLog>(&mut conn)
        .expect("audit rows");
    assert_eq!(audit.len(), 1);
    assert_eq!(schema::events::table.count().get_result::<i64>(&mut conn).unwrap(), 0);
}

#[test]
fn remaining_write_guard_branches_honor_exemption() {
    let insert_schema = branch_schema(
        "CREATE POLICY branch_insert ON branch_items
             AS RESTRICTIVE FOR INSERT WITH CHECK (true);",
    );
    let mut conn = apply_with_exemption(&insert_schema);
    assert!(
        diesel::insert_into(schema::branch_items::table)
            .values((
                schema::branch_items::id.eq(1),
                schema::branch_items::owner.eq("alice"),
                schema::branch_items::body.eq("blocked"),
            ))
            .execute(&mut conn)
            .is_err()
    );
    set_write_exempt(true);
    diesel::insert_into(schema::branch_items::table)
        .values((
            schema::branch_items::id.eq(1),
            schema::branch_items::owner.eq("alice"),
            schema::branch_items::body.eq("restored"),
        ))
        .execute(&mut conn)
        .expect("exempt restrictive-only insert");
    set_write_exempt(false);

    for case in [
        UpdateGuardCase {
            name: "restrictive-only update",
            policies: "CREATE POLICY branch_insert ON branch_items FOR INSERT WITH CHECK (true);
                       CREATE POLICY branch_update ON branch_items
                           AS RESTRICTIVE FOR UPDATE USING (true) WITH CHECK (true);",
            seed_owner: "alice",
            update_owner: "alice",
        },
        UpdateGuardCase {
            name: "update without an applicable policy",
            policies: "CREATE POLICY branch_insert ON branch_items
                           FOR INSERT WITH CHECK (true);",
            seed_owner: "alice",
            update_owner: "alice",
        },
        UpdateGuardCase {
            name: "allow-all using and expression check",
            policies: "CREATE POLICY branch_insert ON branch_items FOR INSERT WITH CHECK (true);
                       CREATE POLICY branch_update ON branch_items
                           FOR UPDATE USING (true) WITH CHECK (owner = 'alice');",
            seed_owner: "alice",
            update_owner: "bob",
        },
        UpdateGuardCase {
            name: "expression using and allow-all check",
            policies: "CREATE POLICY branch_insert ON branch_items FOR INSERT WITH CHECK (true);
                       CREATE POLICY branch_update ON branch_items
                           FOR UPDATE USING (owner = 'alice') WITH CHECK (true);",
            seed_owner: "bob",
            update_owner: "bob",
        },
    ] {
        assert_update_guard_case(&case);
    }
}

#[test]
fn exemption_covers_backing_and_view_insert() {
    let mut conn = apply_with_exemption(SHARED_READ_OWNER_WRITE_SCHEMA);
    set_session_username("alice");

    assert!(
        diesel::insert_into(schema::shared_items_rls::table)
            .values((
                schema::shared_items_rls::id.eq(1),
                schema::shared_items_rls::owner.eq("bob"),
                schema::shared_items_rls::body.eq("blocked"),
            ))
            .execute(&mut conn)
            .is_err(),
        "a normal backing write must remain guarded"
    );

    set_write_exempt(true);
    diesel::insert_into(schema::shared_items_rls::table)
        .values((
            schema::shared_items_rls::id.eq(1),
            schema::shared_items_rls::owner.eq("bob"),
            schema::shared_items_rls::body.eq("server"),
        ))
        .execute(&mut conn)
        .expect("an exempt backing write must land");
    assert_eq!(
        schema::shared_items::table
            .select(schema::shared_items::id)
            .load::<i32>(&mut conn)
            .expect("read the policy view"),
        vec![1],
        "the SELECT policy still decides visibility"
    );

    run_dml(
        &mut conn,
        "INSERT INTO shared_items (id, owner, body) VALUES (2, 'bob', 'local')",
        SHARED_READ_OWNER_WRITE_SCHEMA,
        &exemption_opts(),
    )
    .expect("the exemption covers a view insert");
    assert_eq!(
        schema::shared_items::table
            .select(schema::shared_items::id)
            .order(schema::shared_items::id)
            .load::<i32>(&mut conn)
            .expect("read exempt view rows"),
        vec![1, 2]
    );
}

#[test]
fn exemption_allows_backing_update_only_while_active() {
    let mut conn = apply_with_exemption(GUARDED_SCHEMA);
    set_session_username("alice");
    diesel::insert_into(schema::items_rls::table)
        .values(ItemRow { id: 1, owner: "bob".to_owned(), body: "old".to_owned() })
        .execute(&mut conn)
        .expect("seed bob row");

    assert!(
        diesel::update(schema::items_rls::table.filter(schema::items_rls::id.eq(1)))
            .set(schema::items_rls::body.eq("blocked"))
            .execute(&mut conn)
            .is_err(),
        "a normal backing update must remain guarded"
    );

    set_write_exempt(true);
    diesel::update(schema::items_rls::table.filter(schema::items_rls::id.eq(1)))
        .set(schema::items_rls::body.eq("server"))
        .execute(&mut conn)
        .expect("an exempt backing update must land");
    assert_eq!(
        schema::items_rls::table
            .find(1)
            .select(schema::items_rls::body)
            .get_result::<String>(&mut conn)
            .expect("read updated body"),
        "server"
    );
}

#[test]
fn nested_ownership_policy_uses_exemption_only_at_the_backing_guard() {
    let mut conn = apply_with_exemption(NESTED_OWNERSHIP_SCHEMA);
    set_session_username("alice");
    diesel::insert_into(schema::item_access::table)
        .values((
            schema::item_access::item_id.eq(1),
            schema::item_access::reader.eq("alice"),
            schema::item_access::writer.eq("bob"),
        ))
        .execute(&mut conn)
        .expect("seed ownership");

    assert!(
        diesel::insert_into(schema::nested_items_rls::table)
            .values(
                (schema::nested_items_rls::id.eq(1), schema::nested_items_rls::owner.eq("bob"),)
            )
            .execute(&mut conn)
            .is_err(),
        "the nested write policy must reject a normal backing write"
    );

    set_write_exempt(true);
    diesel::insert_into(schema::nested_items_rls::table)
        .values((schema::nested_items_rls::id.eq(1), schema::nested_items_rls::owner.eq("bob")))
        .execute(&mut conn)
        .expect("the exempt backing write must land");
    assert_eq!(
        schema::nested_items::table
            .select(schema::nested_items::id)
            .load::<i32>(&mut conn)
            .expect("read nested policy view"),
        vec![1],
        "the nested SELECT policy still evaluates as alice"
    );
}

#[test]
fn exempt_write_keeps_constraints_and_unrelated_triggers_active() {
    let mut conn = apply_with_exemption(SHARED_READ_OWNER_WRITE_SCHEMA);
    set_session_username("alice");
    diesel::connection::SimpleConnection::batch_execute(
        &mut conn,
        "CREATE TABLE side_effects (id INTEGER PRIMARY KEY);
         CREATE TRIGGER record_shared_item AFTER INSERT ON shared_items_rls
         BEGIN INSERT INTO side_effects (id) VALUES (NEW.id); END;",
    )
    .expect("install unrelated trigger");
    set_write_exempt(true);

    diesel::insert_into(schema::shared_items_rls::table)
        .values((
            schema::shared_items_rls::id.eq(1),
            schema::shared_items_rls::owner.eq("bob"),
            schema::shared_items_rls::body.eq("server"),
        ))
        .execute(&mut conn)
        .expect("the exempt write must land");
    assert_eq!(
        schema::side_effects::table
            .select(schema::side_effects::id)
            .load::<i32>(&mut conn)
            .expect("read trigger side effect"),
        vec![1],
        "the unrelated trigger must still run"
    );
    assert!(
        diesel::insert_into(schema::shared_items_rls::table)
            .values((
                schema::shared_items_rls::id.eq(1),
                schema::shared_items_rls::owner.eq("bob"),
                schema::shared_items_rls::body.eq("duplicate"),
            ))
            .execute(&mut conn)
            .is_err(),
        "the exemption must not bypass physical uniqueness"
    );
}

#[test]
fn translated_trigger_view_write_inherits_exemption() {
    let mut conn = apply_with_exemption(CASCADED_RLS_SCHEMA);
    set_session_username("alice");
    set_write_exempt(true);

    diesel::insert_into(schema::parent_items_rls::table)
        .values((schema::parent_items_rls::id.eq(1), schema::parent_items_rls::owner.eq("bob")))
        .execute(&mut conn)
        .expect("the exempt parent write must land");
    assert_eq!(
        schema::child_items_rls::table
            .select(schema::child_items_rls::id)
            .load::<i32>(&mut conn)
            .expect("read cascaded child"),
        vec![1],
        "the nested backing write must inherit the exemption"
    );
}
