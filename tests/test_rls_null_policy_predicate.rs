//! H3: RLS write guards silently pass when the policy predicate evaluates to
//! NULL.
//!
//! PostgreSQL requires WITH CHECK to evaluate to exactly TRUE, treating NULL as
//! denial. The emitted INSTEAD OF INSERT trigger uses
//! `WHERE NOT (predicate)`, which is NULL when the policy operand is NULL, so
//! the RAISE never fires and the row lands. The INSTEAD OF UPDATE trigger has
//! the same shape: `WHERE (using) AND NOT (check)` evaluates to NULL when the
//! NEW row makes `check` NULL.
//!
//! These tests pin the correct PostgreSQL behavior: INSERT or UPDATE whose
//! WITH CHECK evaluates to NULL must be refused.

use diesel::{prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};

#[declare_sql_function]
extern "SQL" {
    /// Returns the session user name mapped from current_setting('app.user').
    fn app_user() -> diesel::sql_types::Text;
}

mod schema {
    diesel::table! {
        docs (id) {
            id -> Integer,
            owner -> Nullable<Text>,
            body -> Nullable<Text>,
        }
    }

    diesel::table! {
        docs_rls (id) {
            id -> Integer,
            owner -> Nullable<Text>,
            body -> Nullable<Text>,
        }
    }
}

use schema::{docs, docs_rls};

#[derive(Insertable)]
#[diesel(table_name = docs_rls)]
struct SeedRow {
    id: i32,
    owner: Option<String>,
    body: Option<String>,
}

const PG: &str = "\
CREATE TABLE docs (
    id INTEGER PRIMARY KEY,
    owner TEXT,
    body TEXT
);
ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
CREATE POLICY p ON docs FOR ALL
    USING (owner = current_setting('app.user'))
    WITH CHECK (owner = current_setting('app.user'));
";

fn opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_violations")
        .with_session_variable(SessionVariableMapping::current_setting("app.user", "app_user"))
        .with_user_defined_functions(["app_user"])
}

fn apply() -> SqliteConnection {
    let stmts = Pg2Sqlite::default().sql(PG).expect("parse").translate(&opts()).expect("translate");
    let mut conn = SqliteConnection::establish(":memory:").expect("open db");
    // app_user() maps current_setting('app.user'). Returns a static string so
    // the closure is 'static.
    app_user_utils::register_impl(&mut conn, || "alice".to_string()).expect("register app_user");
    for s in &stmts {
        // DDL (CREATE TABLE, CREATE VIEW, CREATE TRIGGER) cannot be expressed
        // in Diesel's typed DSL.
        diesel::sql_query(s.to_string()).execute(&mut conn).expect("apply DDL");
    }
    conn
}

/// PostgreSQL treats NULL from WITH CHECK as denial for INSERT. When the owner
/// column is omitted the value is NULL, making
/// `NULL = current_setting('app.user')` evaluate to NULL. PostgreSQL raises
/// "new row violates row-level security policy". The emitted trigger uses
/// `WHERE NOT (NULL)`, which is itself NULL and therefore never fires, so the
/// row lands in the backing table silently.
#[test]
fn insert_with_null_owner_must_be_refused() {
    let mut conn = apply();
    // owner omitted: the column receives NULL. The INSTEAD OF INSERT trigger
    // guard is `WHERE NOT (NEW.owner = app_user())` = `WHERE NOT NULL` = no
    // match, so RAISE does not fire and the insert proceeds.
    let result = diesel::insert_into(docs::table)
        .values((docs::id.eq(2i32), docs::body.eq("secret")))
        .execute(&mut conn);
    assert!(
        result.is_err(),
        "INSERT with NULL owner must be refused: NULL WITH CHECK is denial, got Ok"
    );
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("row-level security"), "refusal must name the RLS policy, got: {msg}");
}

/// PostgreSQL treats NULL from WITH CHECK as denial for UPDATE too. Setting
/// owner to NULL makes `NULL = current_setting('app.user')` NULL. The emitted
/// UPDATE trigger guard is
/// `WHERE (OLD.owner = app_user()) AND NOT (NEW.owner = app_user())`.
/// With OLD.owner = 'alice' (true) and NEW.owner = NULL (NULL), the AND
/// evaluates to NULL, so RAISE does not fire and the update lands.
#[test]
fn update_setting_owner_to_null_must_be_refused() {
    let mut conn = apply();
    // Seed alice's row directly into the backing table to bypass view guards
    // for setup only.
    diesel::insert_into(docs_rls::table)
        .values(SeedRow { id: 1, owner: Some("alice".to_owned()), body: Some("body".to_owned()) })
        .execute(&mut conn)
        .expect("seed");

    // The UPDATE sets owner to NULL. WITH CHECK (NULL = 'alice') is NULL.
    // PostgreSQL raises. The current trigger emits
    // `WHERE (true) AND NOT (NULL)` = `WHERE NULL`, so RAISE never fires.
    let result = diesel::update(docs::table.filter(docs::id.eq(1i32)))
        .set(docs::owner.eq(Option::<String>::None))
        .execute(&mut conn);
    assert!(
        result.is_err(),
        "UPDATE setting owner to NULL must be refused: NULL WITH CHECK is denial, got Ok"
    );
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("row-level security"), "refusal must name the RLS policy, got: {msg}");
}

/// Companion green test: INSERT with owner matching the session user succeeds.
/// Proves the schema is correctly set up and the trigger only fires for the
/// NULL case.
#[test]
fn insert_with_matching_owner_succeeds() {
    let mut conn = apply();
    diesel::insert_into(docs::table)
        .values((docs::id.eq(1i32), docs::owner.eq("alice"), docs::body.eq("ok")))
        .execute(&mut conn)
        .expect("INSERT satisfying the policy must succeed");
    let count: i64 = docs_rls::table.count().get_result(&mut conn).expect("count");
    assert_eq!(count, 1, "the row must land in the backing table");
}
