//! M2: Role-scoped policies (TO role) apply to everyone.
//!
//! Per Decision 3: `filter_policies` must honor `with_session_user_role`. A
//! policy scoped `TO admin` must not apply when the configured session role is
//! `app_user`. Without `with_session_user_role` set, a batch that contains any
//! non-PUBLIC role must refuse translation naming the option.
//!
//! The defect: `filter_policies` never consults `policy.roles()`, so a
//! permissive admin-scoped `USING (id > 0)` widens every user's view to all
//! rows, turning access control inside out.

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
            owner -> Text,
            body -> Nullable<Text>,
        }
    }

    diesel::table! {
        docs_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Nullable<Text>,
        }
    }
}

use schema::{docs, docs_rls};

#[derive(Insertable)]
#[diesel(table_name = docs_rls)]
struct SeedRow {
    id: i32,
    owner: String,
    body: Option<String>,
}

// admin_read is scoped TO admin and has USING (id > 0). user_read is unscoped
// (public) and filters by owner. With with_session_user_role("app_user"):
// admin_read should NOT apply (admin != app_user), so alice sees only her rows.
const PG: &str = "\
CREATE TABLE docs (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    body TEXT
);
ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
CREATE POLICY docs_ins ON docs FOR INSERT WITH CHECK (true);
CREATE POLICY admin_read ON docs TO admin USING (id > 0);
CREATE POLICY user_read ON docs USING (owner = current_setting('app.user'));
";

fn opts_with_role() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_violations")
        .with_session_variable(SessionVariableMapping::current_setting("app.user", "app_user"))
        .with_user_defined_functions(["app_user"])
        .with_session_user_role("app_user".to_string())
}

fn opts_without_role() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_violations")
        .with_session_variable(SessionVariableMapping::current_setting("app.user", "app_user"))
        .with_user_defined_functions(["app_user"])
    // no with_session_user_role
}

fn apply(opts: &Pg2SqliteOptions) -> SqliteConnection {
    let stmts = Pg2Sqlite::default().sql(PG).expect("parse").translate(opts).expect("translate");
    let mut conn = SqliteConnection::establish(":memory:").expect("open db");
    app_user_utils::register_impl(&conn, || "alice".to_string()).expect("register app_user");
    for s in &stmts {
        // DDL (CREATE TABLE, CREATE VIEW, CREATE TRIGGER) cannot be expressed
        // in Diesel's typed DSL.
        diesel::sql_query(s.to_string()).execute(&mut conn).expect("apply DDL");
    }
    conn
}

fn seed(conn: &mut SqliteConnection) {
    diesel::insert_into(docs_rls::table)
        .values(vec![
            SeedRow { id: 1, owner: "alice".to_owned(), body: Some("alice body".to_owned()) },
            SeedRow { id: 2, owner: "bob".to_owned(), body: Some("bob body".to_owned()) },
        ])
        .execute(conn)
        .expect("seed");
}

/// When with_session_user_role("app_user") is set, a policy scoped TO admin
/// must not apply for the app_user role. Only the unscoped (public) policy
/// applies, so alice (app_user) sees only her own rows.
///
/// Currently filter_policies ignores policy.roles(), so admin_read's
/// USING (id > 0) is ORed in for everyone, making both rows visible.
#[test]
fn role_scoped_policy_excluded_when_session_role_mismatch() {
    let mut conn = apply(&opts_with_role());
    seed(&mut conn);

    // Expected (fixed): only user_read applies for role app_user.
    // View filter: WHERE (owner = 'alice'). Alice sees 1 row.
    // Current (buggy): admin_read included for everyone.
    // View filter: WHERE (id > 0) OR (owner = 'alice') = WHERE (id > 0). Both rows
    // visible.
    let count: i64 = docs::table.count().get_result(&mut conn).expect("count");
    assert_eq!(
        count, 1,
        "alice must see only her own rows when admin policy is excluded by role, got {count}"
    );
}

/// Without with_session_user_role, a batch containing a non-PUBLIC TO clause
/// must refuse translation naming the option so the caller knows what to set.
/// Currently filter_policies ignores roles and translates successfully.
#[test]
fn translation_refuses_role_scoped_policy_without_session_user_role() {
    // This call must return Err (naming the missing option). Currently it
    // returns Ok because filter_policies never checks policy.roles().
    let result = Pg2Sqlite::default().sql(PG).expect("parse").translate(&opts_without_role());
    assert!(
        result.is_err(),
        "translation must refuse a TO-scoped policy when with_session_user_role is not set, \
         currently succeeds"
    );
}
