//! `ON CONFLICT` against a policy-bearing view was forwarded to that view,
//! and SQLite refused with "cannot UPSERT a view".
//!
//! In strict validation mode the insert is redirected at the backing table,
//! where the conflict can be evaluated and the `BEFORE INSERT` guard still
//! enforces the policy. In default mode the translator refuses immediately,
//! because it emits no backing-table guard and a redirect there would write
//! past the policy.
//!
//! `INSERT OR IGNORE` and `INSERT OR REPLACE` are accepted by SQLite on a view
//! and are left untouched by the translator.

mod helpers;

use diesel::prelude::*;
use helpers::{establish_connection, set_session_username};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, TranslationOptions};

mod schema {
    diesel::table! {
        /// The backing table: read directly so assertions see what was stored.
        items_rls (id) {
            id -> Integer,
            owner -> Text,
            val -> Integer,
        }
    }

    diesel::table! {
        /// A policy-free table for regression coverage.
        things (id) {
            id -> Integer,
            val -> Integer,
        }
    }
}

use schema::{items_rls, things};

/// Policy compares the owner column against the current user's name so that
/// TEXT-to-TEXT comparisons work without UUID/BLOB casting.
const SCHEMA: &str = "
    CREATE TABLE items (
        id INTEGER PRIMARY KEY,
        owner TEXT NOT NULL,
        val INTEGER NOT NULL DEFAULT 0
    );
    ALTER TABLE items ENABLE ROW LEVEL SECURITY;
    CREATE POLICY items_select ON items FOR SELECT USING (owner = current_user);
    CREATE POLICY items_write ON items FOR INSERT WITH CHECK (owner = current_user);
    CREATE POLICY items_update ON items FOR UPDATE USING (owner = current_user);
";

const NO_POLICY_SCHEMA: &str = "
    CREATE TABLE things (
        id INTEGER PRIMARY KEY,
        val INTEGER NOT NULL DEFAULT 0
    );
";

fn strict_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_strict_rls_validation()
        .with_session_variable(SessionVariableMapping::current_user("current_app_username"))
}

fn default_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::current_user("current_app_username"))
}

/// Applies translated DDL to a fresh in-memory connection. The emitted SQL is
/// the artifact under test, so it runs as generated text; every other
/// statement uses the typed DSL.
fn apply(pg: &str, opts: &Pg2SqliteOptions) -> SqliteConnection {
    let translated =
        Pg2Sqlite::default().sql(pg).expect("parse").translate(opts).expect("translate");
    let mut conn = establish_connection();
    for statement in &translated {
        diesel::sql_query(statement.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("emitted DDL failed: {e}\n{statement}"));
    }
    conn
}

/// Translates `pg_dml` in the context of `schema` and executes the result.
///
/// The emitted upsert SQL is the artifact under test; diesel's typed DSL
/// bypasses the translator and cannot verify what pg2sqlite generates.
fn run_dml(
    conn: &mut SqliteConnection,
    pg_dml: &str,
    schema: &str,
    opts: &Pg2SqliteOptions,
) -> QueryResult<usize> {
    let schema_count = Pg2Sqlite::default()
        .sql(schema)
        .expect("parse")
        .translate(opts)
        .expect("translate schema")
        .len();
    let combined = format!("{schema}\n{pg_dml}");
    let all = Pg2Sqlite::default()
        .sql(&combined)
        .expect("parse")
        .translate(opts)
        .expect("translate combined");
    let mut last = 0;
    for stmt in all.into_iter().skip(schema_count) {
        last = diesel::sql_query(stmt.to_string()).execute(conn)?;
    }
    Ok(last)
}

/// Translates `pg_dml` in the context of `schema` and returns the translation
/// error, panicking when translation unexpectedly succeeds.
fn dml_translation_err(pg_dml: &str, schema: &str, opts: &Pg2SqliteOptions) -> String {
    let combined = format!("{schema}\n{pg_dml}");
    Pg2Sqlite::default()
        .sql(&combined)
        .expect("parse")
        .translate(opts)
        .expect_err("expected translation error")
        .to_string()
}

// ---------------------------------------------------------------------------
// Strict mode: ON CONFLICT redirected to the backing table
// ---------------------------------------------------------------------------

/// Before the fix, the translated SQL still targeted the view and SQLite
/// refused with "cannot UPSERT a view". After the fix it targets items_rls
/// and the DO NOTHING is handled at the real table level.
#[test]
fn strict_do_nothing_leaves_existing_row_unchanged() {
    set_session_username("alice");
    let mut conn = apply(SCHEMA, &strict_opts());
    diesel::insert_into(items_rls::table)
        .values((items_rls::id.eq(1), items_rls::owner.eq("alice"), items_rls::val.eq(10)))
        .execute(&mut conn)
        .expect("seed backing table");

    run_dml(
        &mut conn,
        "INSERT INTO items (id, owner, val) VALUES (1, 'alice', 20) ON CONFLICT (id) DO NOTHING",
        SCHEMA,
        &strict_opts(),
    )
    .expect("DO NOTHING on a conflicting row must succeed");

    let val: i32 = items_rls::table
        .select(items_rls::val)
        .filter(items_rls::id.eq(1))
        .first(&mut conn)
        .expect("row must exist in backing table");
    assert_eq!(val, 10, "DO NOTHING must leave the existing value unchanged");
}

/// Before the fix, the translated SQL still targeted the view and SQLite
/// refused with "cannot UPSERT a view". After the fix it targets items_rls
/// and the DO UPDATE lands the new value.
#[test]
fn strict_do_update_stores_the_updated_value() {
    set_session_username("alice");
    let mut conn = apply(SCHEMA, &strict_opts());
    diesel::insert_into(items_rls::table)
        .values((items_rls::id.eq(1), items_rls::owner.eq("alice"), items_rls::val.eq(10)))
        .execute(&mut conn)
        .expect("seed backing table");

    run_dml(
        &mut conn,
        "INSERT INTO items (id, owner, val) VALUES (1, 'alice', 20) \
         ON CONFLICT (id) DO UPDATE SET val = excluded.val",
        SCHEMA,
        &strict_opts(),
    )
    .expect("DO UPDATE on a conflicting row must succeed");

    let val: i32 = items_rls::table
        .select(items_rls::val)
        .filter(items_rls::id.eq(1))
        .first(&mut conn)
        .expect("row must exist in backing table");
    assert_eq!(val, 20, "DO UPDATE must store the new value");
}

/// The BEFORE INSERT guard on the backing table fires before the conflict
/// check, so a policy-violating upsert is caught even when DO NOTHING would
/// otherwise skip the row.
#[test]
fn strict_upsert_policy_violation_is_refused() {
    set_session_username("alice");
    let mut conn = apply(SCHEMA, &strict_opts());
    diesel::insert_into(items_rls::table)
        .values((items_rls::id.eq(1), items_rls::owner.eq("alice"), items_rls::val.eq(10)))
        .execute(&mut conn)
        .expect("seed backing table");

    let err = run_dml(
        &mut conn,
        "INSERT INTO items (id, owner, val) VALUES (1, 'bob', 20) ON CONFLICT (id) DO NOTHING",
        SCHEMA,
        &strict_opts(),
    )
    .expect_err("policy-violating upsert must be refused");
    assert!(
        err.to_string().contains("new row violates row-level security policy"),
        "the BEFORE INSERT guard must refuse the policy-violating row, got: {err}"
    );

    let val: i32 = items_rls::table
        .select(items_rls::val)
        .filter(items_rls::id.eq(1))
        .first(&mut conn)
        .expect("row must exist");
    assert_eq!(val, 10, "the refused upsert must not change the existing row");
}

// ---------------------------------------------------------------------------
// Default mode: ON CONFLICT refused at translation time
// ---------------------------------------------------------------------------

/// Before the fix, translation succeeded and SQLite refused at runtime.
/// After the fix, translation itself refuses with a message naming the clause
/// and the remedy.
#[test]
fn default_mode_do_nothing_is_refused_at_translation() {
    set_session_username("alice");
    let err = dml_translation_err(
        "INSERT INTO items (id, owner, val) VALUES (1, 'alice', 20) ON CONFLICT (id) DO NOTHING",
        SCHEMA,
        &default_opts(),
    );
    assert!(
        err.contains("ON CONFLICT") && err.contains("with_strict_rls_validation"),
        "error must name the clause and the remedy, got: {err}"
    );
}

#[test]
fn default_mode_do_update_is_refused_at_translation() {
    set_session_username("alice");
    let err = dml_translation_err(
        "INSERT INTO items (id, owner, val) VALUES (1, 'alice', 20) \
         ON CONFLICT (id) DO UPDATE SET val = excluded.val",
        SCHEMA,
        &default_opts(),
    );
    assert!(
        err.contains("ON CONFLICT") && err.contains("with_strict_rls_validation"),
        "error must name the clause and the remedy, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// INSERT OR IGNORE / INSERT OR REPLACE still accepted on the view
// ---------------------------------------------------------------------------

/// SQLite accepts INSERT OR IGNORE on views with INSTEAD OF INSERT triggers;
/// this form has no PostgreSQL equivalent and cannot reach the translator as
/// PG input. The typed ON CONFLICT DSL generates upsert syntax that SQLite
/// refuses on views, so only the OR IGNORE form can exercise this path.
#[test]
fn insert_or_ignore_through_view_still_works() {
    set_session_username("alice");
    let mut conn = apply(SCHEMA, &default_opts());

    diesel::sql_query("INSERT OR IGNORE INTO items (id, owner, val) VALUES (1, 'alice', 10)")
        .execute(&mut conn)
        .expect("INSERT OR IGNORE into view must be accepted by SQLite");

    let val: i32 = items_rls::table
        .select(items_rls::val)
        .filter(items_rls::id.eq(1))
        .first(&mut conn)
        .expect("row must exist in backing table");
    assert_eq!(val, 10);
}

/// Same reasoning as INSERT OR IGNORE: OR REPLACE is a SQLite-specific form
/// that SQLite accepts on views, with no typed DSL equivalent that preserves
/// the OR REPLACE semantics on a view.
#[test]
fn insert_or_replace_through_view_still_works() {
    set_session_username("alice");
    let mut conn = apply(SCHEMA, &default_opts());

    diesel::sql_query("INSERT OR REPLACE INTO items (id, owner, val) VALUES (1, 'alice', 10)")
        .execute(&mut conn)
        .expect("INSERT OR REPLACE into view must be accepted by SQLite");

    let val: i32 = items_rls::table
        .select(items_rls::val)
        .filter(items_rls::id.eq(1))
        .first(&mut conn)
        .expect("row must exist in backing table");
    assert_eq!(val, 10);
}

// ---------------------------------------------------------------------------
// Table with no policy: upsert translates and runs in both modes
// ---------------------------------------------------------------------------

#[test]
fn table_without_policy_upsert_in_strict_mode() {
    let mut conn = apply(NO_POLICY_SCHEMA, &strict_opts());
    run_dml(
        &mut conn,
        "INSERT INTO things (id, val) VALUES (1, 10) ON CONFLICT (id) DO NOTHING",
        NO_POLICY_SCHEMA,
        &strict_opts(),
    )
    .expect("upsert on a policy-free table must succeed in strict mode");
    let val: i32 = things::table.select(things::val).first(&mut conn).expect("row");
    assert_eq!(val, 10);
}

#[test]
fn table_without_policy_upsert_in_default_mode() {
    let mut conn = apply(NO_POLICY_SCHEMA, &default_opts());
    run_dml(
        &mut conn,
        "INSERT INTO things (id, val) VALUES (1, 10) ON CONFLICT (id) DO NOTHING",
        NO_POLICY_SCHEMA,
        &default_opts(),
    )
    .expect("upsert on a policy-free table must succeed in default mode");
    let val: i32 = things::table.select(things::val).first(&mut conn).expect("row");
    assert_eq!(val, 10);
}
