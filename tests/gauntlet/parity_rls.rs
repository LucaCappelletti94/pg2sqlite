//! Parity test: five RLS fixtures produce the same access decisions on both
//! engines.
//!
//! For each fixture the test seeds rows through the bypass path (superuser on
//! PostgreSQL, the backing _rls table on SQLite), then drives reads and writes
//! on both engines step by step and asserts they agree. A disagreement is a
//! finding about the translator, reported with the fixture, the step, and both
//! outcomes.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::{
    establish_connection, set_session_department, set_session_user_id, set_session_username,
};
use pg2sqlite::prelude::{
    Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, TranslationOptions, UuidRepresentation,
};
use postgres_harness::Outcome;
use rosetta_uuid::Uuid;

use crate::{helpers, postgres_harness};

// User UUIDs shared across all scenarios.
fn ua() -> Uuid {
    Uuid::from([0x01; 16])
}
fn ub() -> Uuid {
    Uuid::from([0x02; 16])
}
fn uc() -> Uuid {
    Uuid::from([0x03; 16])
}

// ════════════════════════════════════════════════════════════════
// rls_basic: documents owned by a single user, four-policy table.
// ════════════════════════════════════════════════════════════════

const RLS_BASIC: &str = include_str!("../fixtures/rls_basic.sql");

mod basic_schema {
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        documents (id) {
            id -> Uuid,
            owner_id -> Uuid,
            title -> Text,
            content -> Nullable<Text>,
        }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        /// Backing table for the documents view on the SQLite side.
        documents_rls (id) {
            id -> Uuid,
            owner_id -> Uuid,
            title -> Text,
            content -> Nullable<Text>,
        }
    }
}

#[derive(Debug)]
enum BasicStep {
    Session(Uuid),
    InsertDoc { id: Uuid, owner: Uuid, title: &'static str },
    DeleteDoc(Uuid),
    VisibleTitles,
    StoredCount,
}

struct BasicPgRunner {
    connection: PgConnection,
    acting: Uuid,
    current_role: String,
}
struct BasicSqliteRunner {
    connection: SqliteConnection,
    acting: Uuid,
}

macro_rules! impl_basic_step {
    ($t:ty) => {
        impl $t {
            fn step(&mut self, step: &BasicStep) -> Outcome {
                use basic_schema::documents;
                match step {
                    BasicStep::Session(user) => {
                        self.become_user(*user);
                        Outcome::Accepted
                    }
                    BasicStep::InsertDoc { id, owner, title } => {
                        Outcome::of(
                            &diesel::insert_into(documents::table)
                                .values((
                                    documents::id.eq(*id),
                                    documents::owner_id.eq(*owner),
                                    documents::title.eq(*title),
                                    documents::content.eq(Option::<String>::None),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    BasicStep::DeleteDoc(id) => {
                        Outcome::of(
                            &diesel::delete(documents::table.filter(documents::id.eq(*id)))
                                .execute(&mut self.connection),
                        )
                    }
                    BasicStep::VisibleTitles => {
                        let titles: Vec<String> = documents::table
                            .select(documents::title)
                            .order(documents::title.asc())
                            .load(&mut self.connection)
                            .expect("visible titles");
                        Outcome::Rows(titles)
                    }
                    BasicStep::StoredCount => Outcome::Count(self.stored_count()),
                }
            }
        }
    };
}

impl_basic_step!(BasicPgRunner);
impl_basic_step!(BasicSqliteRunner);

impl BasicPgRunner {
    fn become_user(&mut self, user: Uuid) {
        self.acting = user;
        // Vendor-specific session commands.
        diesel::sql_query(format!("SET app.user_id = '{user}'"))
            .execute(&mut self.connection)
            .expect("set session user");
        if self.current_role.is_empty() {
            // Vendor-specific role switch so RLS applies.
            diesel::sql_query("SET ROLE app").execute(&mut self.connection).expect("set role app");
            self.current_role = "app".to_string();
        }
    }

    fn stored_count(&mut self) -> i64 {
        use basic_schema::documents;
        let role = self.current_role.clone();
        // Vendor-specific role reset to count past RLS.
        diesel::sql_query("RESET ROLE").execute(&mut self.connection).expect("reset role");
        let count =
            documents::table.count().get_result(&mut self.connection).expect("count documents");
        if !role.is_empty() {
            diesel::sql_query(format!("SET ROLE {role}"))
                .execute(&mut self.connection)
                .expect("restore role");
        }
        count
    }

    fn seed(&mut self, id: Uuid, owner: Uuid, title: &str) {
        use basic_schema::documents;
        diesel::insert_into(documents::table)
            .values((
                documents::id.eq(id),
                documents::owner_id.eq(owner),
                documents::title.eq(title),
                documents::content.eq(Option::<String>::None),
            ))
            .execute(&mut self.connection)
            .expect("seed document on pg");
    }
}

impl BasicSqliteRunner {
    fn become_user(&mut self, user: Uuid) {
        self.acting = user;
        set_session_user_id(&user);
    }

    fn stored_count(&mut self) -> i64 {
        use basic_schema::documents_rls;
        documents_rls::table.count().get_result(&mut self.connection).expect("count documents_rls")
    }

    fn seed(&mut self, id: Uuid, owner: Uuid, title: &str) {
        use basic_schema::documents_rls;
        diesel::insert_into(documents_rls::table)
            .values((
                documents_rls::id.eq(id),
                documents_rls::owner_id.eq(owner),
                documents_rls::title.eq(title),
                documents_rls::content.eq(Option::<String>::None),
            ))
            .execute(&mut self.connection)
            .expect("seed documents_rls");
    }
}

fn basic_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_session_user_role("authenticated")
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
}

fn basic_scenario() -> Vec<BasicStep> {
    let d1 = Uuid::from([0xd1; 16]);
    let d3 = Uuid::from([0xd3; 16]);
    let d4 = Uuid::from([0xd4; 16]);
    vec![
        BasicStep::Session(ua()),
        BasicStep::VisibleTitles, // ["alice doc"]
        BasicStep::InsertDoc { id: d3, owner: ua(), title: "alice extra" }, // Accepted
        BasicStep::InsertDoc { id: d4, owner: ub(), title: "alice as bob" }, // Refused
        BasicStep::StoredCount,   // 3
        BasicStep::Session(ub()),
        BasicStep::VisibleTitles, // ["bob doc"]
        BasicStep::DeleteDoc(d1), // Accepted (0 rows)
        BasicStep::StoredCount,   // 3 (alice's doc unchanged)
        BasicStep::Session(ua()),
        BasicStep::DeleteDoc(d1), // Accepted
        BasicStep::StoredCount,   // 2
    ]
}

#[test]
fn rls_basic_engines_agree() {
    let mut pg = {
        let mut conn = postgres_harness::fresh_database();
        postgres_harness::apply(&mut conn, RLS_BASIC).expect("apply rls_basic to pg");
        postgres_harness::apply(
            &mut conn,
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO app",
        )
        .expect("grant DML to app");
        BasicPgRunner { connection: conn, acting: ua(), current_role: String::new() }
    };
    let mut sq = {
        let translated = Pg2Sqlite::default()
            .sql(RLS_BASIC)
            .expect("parse rls_basic")
            .translate(&basic_options())
            .expect("translate rls_basic");
        let mut conn = establish_connection();
        for stmt in &translated {
            // DDL migration: raw SQL is the correct form here.
            diesel::sql_query(stmt.to_string())
                .execute(&mut conn)
                .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
        }
        BasicSqliteRunner { connection: conn, acting: ua() }
    };

    // Set a session user before seeds so the _rls audit triggers can query the
    // view.
    set_session_user_id(&ua());
    let d1 = Uuid::from([0xd1; 16]);
    let d2 = Uuid::from([0xd2; 16]);
    pg.seed(d1, ua(), "alice doc");
    sq.seed(d1, ua(), "alice doc");
    pg.seed(d2, ub(), "bob doc");
    sq.seed(d2, ub(), "bob doc");

    for step in basic_scenario() {
        let expected = pg.step(&step);
        let actual = sq.step(&step);
        assert_eq!(actual, expected, "rls_basic disagrees on {step:?}");
    }
}

// ════════════════════════════════════════════════════════════════════════
// rls_multiple_policies: three OR'd SELECT policies, owner-only writes.
// ════════════════════════════════════════════════════════════════════════

const RLS_MULTIPLE_POLICIES: &str = include_str!("../fixtures/rls_multiple_policies.sql");

mod multi_schema {
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        documents (id) {
            id -> Uuid,
            owner_id -> Uuid,
            is_public -> Bool,
            department -> Nullable<Text>,
            title -> Text,
        }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        /// Backing table for the documents view on the SQLite side.
        documents_rls (id) {
            id -> Uuid,
            owner_id -> Uuid,
            is_public -> Bool,
            department -> Nullable<Text>,
            title -> Text,
        }
    }
}

#[derive(Debug)]
enum MultiStep {
    Session(Uuid),
    Department(Option<&'static str>),
    InsertDoc {
        id: Uuid,
        owner: Uuid,
        is_public: bool,
        department: Option<&'static str>,
        title: &'static str,
    },
    DeleteDoc(Uuid),
    VisibleTitles,
    StoredCount,
}

struct MultiPgRunner {
    connection: PgConnection,
    acting: Uuid,
    current_role: String,
}
struct MultiSqliteRunner {
    connection: SqliteConnection,
    acting: Uuid,
}

macro_rules! impl_multi_step {
    ($t:ty) => {
        impl $t {
            fn step(&mut self, step: &MultiStep) -> Outcome {
                use multi_schema::documents;
                match step {
                    MultiStep::Session(user) => {
                        self.become_user(*user);
                        Outcome::Accepted
                    }
                    MultiStep::Department(opt_dept) => {
                        self.set_department(*opt_dept);
                        Outcome::Accepted
                    }
                    MultiStep::InsertDoc { id, owner, is_public, department, title } => {
                        Outcome::of(
                            &diesel::insert_into(documents::table)
                                .values((
                                    documents::id.eq(*id),
                                    documents::owner_id.eq(*owner),
                                    documents::is_public.eq(*is_public),
                                    documents::department.eq(*department),
                                    documents::title.eq(*title),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    MultiStep::DeleteDoc(id) => {
                        Outcome::of(
                            &diesel::delete(documents::table.filter(documents::id.eq(*id)))
                                .execute(&mut self.connection),
                        )
                    }
                    MultiStep::VisibleTitles => {
                        let titles: Vec<String> = documents::table
                            .select(documents::title)
                            .order(documents::title.asc())
                            .load(&mut self.connection)
                            .expect("visible titles");
                        Outcome::Rows(titles)
                    }
                    MultiStep::StoredCount => Outcome::Count(self.stored_count()),
                }
            }
        }
    };
}

impl_multi_step!(MultiPgRunner);
impl_multi_step!(MultiSqliteRunner);

impl MultiPgRunner {
    fn become_user(&mut self, user: Uuid) {
        self.acting = user;
        // Vendor-specific session commands.
        diesel::sql_query(format!("SET app.user_id = '{user}'"))
            .execute(&mut self.connection)
            .expect("set session user");
        if self.current_role.is_empty() {
            diesel::sql_query("SET ROLE app").execute(&mut self.connection).expect("set role app");
            self.current_role = "app".to_string();
        }
    }

    fn set_department(&mut self, opt_dept: Option<&str>) {
        let sql = match opt_dept {
            Some(d) => format!("SET app.user_department = '{d}'"),
            None => "RESET app.user_department".to_string(),
        };
        // Vendor-specific session GUC command.
        diesel::sql_query(sql).execute(&mut self.connection).expect("set department");
    }

    fn stored_count(&mut self) -> i64 {
        use multi_schema::documents;
        let role = self.current_role.clone();
        diesel::sql_query("RESET ROLE").execute(&mut self.connection).expect("reset role");
        let count =
            documents::table.count().get_result(&mut self.connection).expect("count documents");
        if !role.is_empty() {
            diesel::sql_query(format!("SET ROLE {role}"))
                .execute(&mut self.connection)
                .expect("restore role");
        }
        count
    }

    fn seed(
        &mut self,
        id: Uuid,
        owner: Uuid,
        is_public: bool,
        department: Option<&str>,
        title: &str,
    ) {
        use multi_schema::documents;
        diesel::insert_into(documents::table)
            .values((
                documents::id.eq(id),
                documents::owner_id.eq(owner),
                documents::is_public.eq(is_public),
                documents::department.eq(department),
                documents::title.eq(title),
            ))
            .execute(&mut self.connection)
            .expect("seed document on pg");
    }
}

impl MultiSqliteRunner {
    fn become_user(&mut self, user: Uuid) {
        self.acting = user;
        set_session_user_id(&user);
    }

    fn set_department(&mut self, opt_dept: Option<&str>) {
        let _ = self;
        set_session_department(opt_dept);
    }

    fn stored_count(&mut self) -> i64 {
        use multi_schema::documents_rls;
        documents_rls::table.count().get_result(&mut self.connection).expect("count documents_rls")
    }

    fn seed(
        &mut self,
        id: Uuid,
        owner: Uuid,
        is_public: bool,
        department: Option<&str>,
        title: &str,
    ) {
        use multi_schema::documents_rls;
        diesel::insert_into(documents_rls::table)
            .values((
                documents_rls::id.eq(id),
                documents_rls::owner_id.eq(owner),
                documents_rls::is_public.eq(is_public),
                documents_rls::department.eq(department),
                documents_rls::title.eq(title),
            ))
            .execute(&mut self.connection)
            .expect("seed documents_rls");
    }
}

fn multi_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_session_user_role("authenticated")
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_department",
            "current_app_department",
        ))
}

fn multi_scenario() -> Vec<MultiStep> {
    let d4 = Uuid::from([0xe4; 16]);
    let d5 = Uuid::from([0xe5; 16]);
    vec![
        MultiStep::Session(ua()),
        // No department: ua sees own + public docs.
        MultiStep::VisibleTitles, // ["alice doc", "bob public"]
        MultiStep::Department(Some("eng")),
        // Department "eng": ua also sees carol_eng.
        MultiStep::VisibleTitles, // ["alice doc", "bob public", "carol eng"]
        MultiStep::Department(None),
        MultiStep::InsertDoc {
            id: d4,
            owner: ua(),
            is_public: false,
            department: None,
            title: "alice extra",
        }, // Accepted
        MultiStep::InsertDoc {
            id: d5,
            owner: ub(),
            is_public: false,
            department: None,
            title: "alice as bob",
        }, // Refused
        MultiStep::StoredCount, // 4
        MultiStep::Session(ub()),
        MultiStep::VisibleTitles,                     // ["bob public"]
        MultiStep::DeleteDoc(Uuid::from([0xe1; 16])), // alice's doc: Accepted (0 rows)
        MultiStep::StoredCount,                       // 4 (unchanged)
        MultiStep::DeleteDoc(Uuid::from([0xe2; 16])), // bob's own: Accepted
        MultiStep::StoredCount,                       // 3
    ]
}

#[test]
fn rls_multiple_policies_engines_agree() {
    let mut pg = {
        let mut conn = postgres_harness::fresh_database();
        postgres_harness::apply(&mut conn, RLS_MULTIPLE_POLICIES)
            .expect("apply rls_multiple_policies to pg");
        postgres_harness::apply(
            &mut conn,
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO app",
        )
        .expect("grant DML to app");
        MultiPgRunner { connection: conn, acting: ua(), current_role: String::new() }
    };
    let mut sq = {
        let translated = Pg2Sqlite::default()
            .sql(RLS_MULTIPLE_POLICIES)
            .expect("parse rls_multiple_policies")
            .translate(&multi_options())
            .expect("translate rls_multiple_policies");
        let mut conn = establish_connection();
        for stmt in &translated {
            diesel::sql_query(stmt.to_string())
                .execute(&mut conn)
                .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
        }
        MultiSqliteRunner { connection: conn, acting: ua() }
    };

    // Set a session user before seeds so the _rls audit triggers can query the
    // view.
    set_session_user_id(&ua());
    let d1 = Uuid::from([0xe1; 16]);
    let d2 = Uuid::from([0xe2; 16]);
    let d3 = Uuid::from([0xe3; 16]);
    pg.seed(d1, ua(), false, None, "alice doc");
    sq.seed(d1, ua(), false, None, "alice doc");
    pg.seed(d2, ub(), true, None, "bob public");
    sq.seed(d2, ub(), true, None, "bob public");
    pg.seed(d3, uc(), false, Some("eng"), "carol eng");
    sq.seed(d3, uc(), false, Some("eng"), "carol eng");

    for step in multi_scenario() {
        let expected = pg.step(&step);
        let actual = sq.step(&step);
        assert_eq!(actual, expected, "rls_multiple_policies disagrees on {step:?}");
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// rls_tenant_isolation: subquery-based tenant membership, admin-only deletes.
// ══════════════════════════════════════════════════════════════════════════════

const RLS_TENANT_ISOLATION: &str = include_str!("../fixtures/rls_tenant_isolation.sql");

mod tenant_schema {
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        tenants (id) { id -> Uuid, name -> Text }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        tenant_users (id) {
            id -> Uuid,
            tenant_id -> Uuid,
            user_id -> Uuid,
            role -> Text,
        }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        projects (id) {
            id -> Uuid,
            tenant_id -> Uuid,
            name -> Text,
            created_by -> Uuid,
        }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        /// Backing table for the projects view on the SQLite side.
        projects_rls (id) {
            id -> Uuid,
            tenant_id -> Uuid,
            name -> Text,
            created_by -> Uuid,
        }
    }
}

#[derive(Debug)]
enum TenantStep {
    Session(Uuid),
    InsertProject { id: Uuid, tenant: Uuid, name: &'static str, creator: Uuid },
    DeleteProject(Uuid),
    VisibleNames,
    StoredCount,
}

struct TenantPgRunner {
    connection: PgConnection,
    acting: Uuid,
    current_role: String,
}
struct TenantSqliteRunner {
    connection: SqliteConnection,
    acting: Uuid,
}

macro_rules! impl_tenant_step {
    ($t:ty) => {
        impl $t {
            fn step(&mut self, step: &TenantStep) -> Outcome {
                use tenant_schema::projects;
                match step {
                    TenantStep::Session(user) => {
                        self.become_user(*user);
                        Outcome::Accepted
                    }
                    TenantStep::InsertProject { id, tenant, name, creator } => {
                        Outcome::of(
                            &diesel::insert_into(projects::table)
                                .values((
                                    projects::id.eq(*id),
                                    projects::tenant_id.eq(*tenant),
                                    projects::name.eq(*name),
                                    projects::created_by.eq(*creator),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    TenantStep::DeleteProject(id) => {
                        Outcome::of(
                            &diesel::delete(projects::table.filter(projects::id.eq(*id)))
                                .execute(&mut self.connection),
                        )
                    }
                    TenantStep::VisibleNames => {
                        let names: Vec<String> = projects::table
                            .select(projects::name)
                            .order(projects::name.asc())
                            .load(&mut self.connection)
                            .expect("visible project names");
                        Outcome::Rows(names)
                    }
                    TenantStep::StoredCount => Outcome::Count(self.stored_count()),
                }
            }
        }
    };
}

impl_tenant_step!(TenantPgRunner);
impl_tenant_step!(TenantSqliteRunner);

impl TenantPgRunner {
    fn become_user(&mut self, user: Uuid) {
        self.acting = user;
        diesel::sql_query(format!("SET app.user_id = '{user}'"))
            .execute(&mut self.connection)
            .expect("set session user");
        if self.current_role.is_empty() {
            diesel::sql_query("SET ROLE app").execute(&mut self.connection).expect("set role app");
            self.current_role = "app".to_string();
        }
    }

    fn stored_count(&mut self) -> i64 {
        use tenant_schema::projects;
        let role = self.current_role.clone();
        diesel::sql_query("RESET ROLE").execute(&mut self.connection).expect("reset role");
        let count =
            projects::table.count().get_result(&mut self.connection).expect("count projects");
        if !role.is_empty() {
            diesel::sql_query(format!("SET ROLE {role}"))
                .execute(&mut self.connection)
                .expect("restore role");
        }
        count
    }

    fn seed_tenant(&mut self, id: Uuid, name: &str) {
        use tenant_schema::tenants;
        diesel::insert_into(tenants::table)
            .values((tenants::id.eq(id), tenants::name.eq(name)))
            .execute(&mut self.connection)
            .expect("seed tenant");
    }

    fn seed_membership(&mut self, id: Uuid, tenant: Uuid, user: Uuid, role: &str) {
        use tenant_schema::tenant_users;
        diesel::insert_into(tenant_users::table)
            .values((
                tenant_users::id.eq(id),
                tenant_users::tenant_id.eq(tenant),
                tenant_users::user_id.eq(user),
                tenant_users::role.eq(role),
            ))
            .execute(&mut self.connection)
            .expect("seed tenant_user");
    }

    fn seed_project(&mut self, id: Uuid, tenant: Uuid, name: &str, creator: Uuid) {
        use tenant_schema::projects;
        diesel::insert_into(projects::table)
            .values((
                projects::id.eq(id),
                projects::tenant_id.eq(tenant),
                projects::name.eq(name),
                projects::created_by.eq(creator),
            ))
            .execute(&mut self.connection)
            .expect("seed project on pg");
    }
}

impl TenantSqliteRunner {
    fn become_user(&mut self, user: Uuid) {
        self.acting = user;
        set_session_user_id(&user);
    }

    fn stored_count(&mut self) -> i64 {
        use tenant_schema::projects_rls;
        projects_rls::table.count().get_result(&mut self.connection).expect("count projects_rls")
    }

    fn seed_tenant(&mut self, id: Uuid, name: &str) {
        use tenant_schema::tenants;
        diesel::insert_into(tenants::table)
            .values((tenants::id.eq(id), tenants::name.eq(name)))
            .execute(&mut self.connection)
            .expect("seed tenant");
    }

    fn seed_membership(&mut self, id: Uuid, tenant: Uuid, user: Uuid, role: &str) {
        use tenant_schema::tenant_users;
        diesel::insert_into(tenant_users::table)
            .values((
                tenant_users::id.eq(id),
                tenant_users::tenant_id.eq(tenant),
                tenant_users::user_id.eq(user),
                tenant_users::role.eq(role),
            ))
            .execute(&mut self.connection)
            .expect("seed tenant_user");
    }

    fn seed_project(&mut self, id: Uuid, tenant: Uuid, name: &str, creator: Uuid) {
        use tenant_schema::projects_rls;
        diesel::insert_into(projects_rls::table)
            .values((
                projects_rls::id.eq(id),
                projects_rls::tenant_id.eq(tenant),
                projects_rls::name.eq(name),
                projects_rls::created_by.eq(creator),
            ))
            .execute(&mut self.connection)
            .expect("seed projects_rls");
    }
}

fn tenant_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_session_user_role("authenticated")
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
}

fn tenant_scenario(t1: Uuid, t2: Uuid, p1: Uuid) -> Vec<TenantStep> {
    let p3 = Uuid::from([0xf3; 16]);
    let p4 = Uuid::from([0xf4; 16]);
    vec![
        TenantStep::Session(ua()), // alice, admin of t1
        TenantStep::VisibleNames,  // ["project alpha"]
        TenantStep::InsertProject { id: p3, tenant: t1, name: "alice extra", creator: ua() }, /* Accepted */
        TenantStep::InsertProject { id: p4, tenant: t2, name: "alice in t2", creator: ua() }, /* Refused */
        TenantStep::StoredCount,                                                              // 3
        TenantStep::Session(uc()),     // carol, admin of t2
        TenantStep::VisibleNames,      // ["project beta"]
        TenantStep::DeleteProject(p1), // Accepted (0 rows: p1 in t1)
        TenantStep::StoredCount,       // 3 (unchanged)
        TenantStep::Session(ub()),     // bob, member of t1
        TenantStep::VisibleNames,      // ["alice extra", "project alpha"]
        TenantStep::DeleteProject(p3), // Accepted (0 rows: bob is member)
        TenantStep::StoredCount,       // 3 (unchanged)
    ]
}

#[test]
fn rls_tenant_isolation_engines_agree() {
    let t1 = Uuid::from([0xf1; 16]);
    let t2 = Uuid::from([0xf2; 16]);
    let tu1 = Uuid::from([0xa1; 16]);
    let tu2 = Uuid::from([0xa2; 16]);
    let tu3 = Uuid::from([0xa3; 16]);
    let p1 = Uuid::from([0xb1; 16]);
    let p2 = Uuid::from([0xb2; 16]);

    let mut pg = {
        let mut conn = postgres_harness::fresh_database();
        postgres_harness::apply(&mut conn, RLS_TENANT_ISOLATION)
            .expect("apply rls_tenant_isolation to pg");
        postgres_harness::apply(
            &mut conn,
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO app",
        )
        .expect("grant DML to app");
        TenantPgRunner { connection: conn, acting: ua(), current_role: String::new() }
    };
    let mut sq = {
        let translated = Pg2Sqlite::default()
            .sql(RLS_TENANT_ISOLATION)
            .expect("parse rls_tenant_isolation")
            .translate(&tenant_options())
            .expect("translate rls_tenant_isolation");
        let mut conn = establish_connection();
        for stmt in &translated {
            diesel::sql_query(stmt.to_string())
                .execute(&mut conn)
                .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
        }
        TenantSqliteRunner { connection: conn, acting: ua() }
    };

    // Set a session user before seeds so the _rls audit triggers can query the
    // view.
    set_session_user_id(&ua());
    pg.seed_tenant(t1, "tenant alpha");
    sq.seed_tenant(t1, "tenant alpha");
    pg.seed_tenant(t2, "tenant beta");
    sq.seed_tenant(t2, "tenant beta");
    pg.seed_membership(tu1, t1, ua(), "admin");
    sq.seed_membership(tu1, t1, ua(), "admin");
    pg.seed_membership(tu2, t1, ub(), "member");
    sq.seed_membership(tu2, t1, ub(), "member");
    pg.seed_membership(tu3, t2, uc(), "admin");
    sq.seed_membership(tu3, t2, uc(), "admin");
    pg.seed_project(p1, t1, "project alpha", ua());
    sq.seed_project(p1, t1, "project alpha", ua());
    pg.seed_project(p2, t2, "project beta", uc());
    sq.seed_project(p2, t2, "project beta", uc());

    for step in tenant_scenario(t1, t2, p1) {
        let expected = pg.step(&step);
        let actual = sq.step(&step);
        assert_eq!(actual, expected, "rls_tenant_isolation disagrees on {step:?}");
    }
}

// ═══════════════════════════════════════════════════════════════════════
// rls_public_access: USING(true) read, TO authenticated insert, items.
// ═══════════════════════════════════════════════════════════════════════

const RLS_PUBLIC_ACCESS: &str = include_str!("../fixtures/rls_public_access.sql");

mod public_schema {
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        categories (id) { id -> Uuid, name -> Text }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        /// Backing table for categories on the SQLite side.
        categories_rls (id) { id -> Uuid, name -> Text }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        items (id) {
            id -> Uuid,
            category_id -> Nullable<Uuid>,
            name -> Text,
            owner_id -> Uuid,
            is_featured -> Bool,
        }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        /// Backing table for items on the SQLite side.
        items_rls (id) {
            id -> Uuid,
            category_id -> Nullable<Uuid>,
            name -> Text,
            owner_id -> Uuid,
            is_featured -> Bool,
        }
    }
}

#[derive(Clone, Copy)]
enum PublicStored {
    Categories,
    Items,
}

#[derive(Debug)]
enum PublicStep {
    Session(Uuid),
    InsertCategory { id: Uuid, name: &'static str },
    InsertItem { id: Uuid, category: Uuid, name: &'static str, owner: Uuid, is_featured: bool },
    VisibleCategories,
    VisibleItems,
    StoredCategories,
    StoredItems,
}

struct PublicPgRunner {
    connection: PgConnection,
    acting: Uuid,
    current_role: String,
}
struct PublicSqliteRunner {
    connection: SqliteConnection,
    acting: Uuid,
}

macro_rules! impl_public_step {
    ($t:ty) => {
        impl $t {
            fn step(&mut self, step: &PublicStep) -> Outcome {
                use public_schema::{categories, items};
                match step {
                    PublicStep::Session(user) => {
                        self.become_user(*user);
                        Outcome::Accepted
                    }
                    PublicStep::InsertCategory { id, name } => {
                        Outcome::of(
                            &diesel::insert_into(categories::table)
                                .values((categories::id.eq(*id), categories::name.eq(*name)))
                                .execute(&mut self.connection),
                        )
                    }
                    PublicStep::InsertItem { id, category, name, owner, is_featured } => {
                        Outcome::of(
                            &diesel::insert_into(items::table)
                                .values((
                                    items::id.eq(*id),
                                    items::category_id.eq(Some(*category)),
                                    items::name.eq(*name),
                                    items::owner_id.eq(*owner),
                                    items::is_featured.eq(*is_featured),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    PublicStep::VisibleCategories => {
                        let names: Vec<String> = categories::table
                            .select(categories::name)
                            .order(categories::name.asc())
                            .load(&mut self.connection)
                            .expect("visible categories");
                        Outcome::Rows(names)
                    }
                    PublicStep::VisibleItems => {
                        let names: Vec<String> = items::table
                            .select(items::name)
                            .order(items::name.asc())
                            .load(&mut self.connection)
                            .expect("visible items");
                        Outcome::Rows(names)
                    }
                    PublicStep::StoredCategories => {
                        Outcome::Count(self.stored_count(PublicStored::Categories))
                    }
                    PublicStep::StoredItems => {
                        Outcome::Count(self.stored_count(PublicStored::Items))
                    }
                }
            }
        }
    };
}

impl_public_step!(PublicPgRunner);
impl_public_step!(PublicSqliteRunner);

impl PublicPgRunner {
    fn become_user(&mut self, user: Uuid) {
        self.acting = user;
        diesel::sql_query(format!("SET app.user_id = '{user}'"))
            .execute(&mut self.connection)
            .expect("set session user");
        if self.current_role.is_empty() {
            // The categories INSERT policy is TO authenticated; use that role.
            diesel::sql_query("SET ROLE authenticated")
                .execute(&mut self.connection)
                .expect("set role authenticated");
            self.current_role = "authenticated".to_string();
        }
    }

    fn stored_count(&mut self, which: PublicStored) -> i64 {
        use public_schema::{categories, items};
        let role = self.current_role.clone();
        diesel::sql_query("RESET ROLE").execute(&mut self.connection).expect("reset role");
        let count = match which {
            PublicStored::Categories => {
                categories::table
                    .count()
                    .get_result(&mut self.connection)
                    .expect("count categories")
            }
            PublicStored::Items => {
                items::table.count().get_result(&mut self.connection).expect("count items")
            }
        };
        if !role.is_empty() {
            diesel::sql_query(format!("SET ROLE {role}"))
                .execute(&mut self.connection)
                .expect("restore role");
        }
        count
    }

    fn seed_category(&mut self, id: Uuid, name: &str) {
        use public_schema::categories;
        diesel::insert_into(categories::table)
            .values((categories::id.eq(id), categories::name.eq(name)))
            .execute(&mut self.connection)
            .expect("seed category on pg");
    }

    fn seed_item(&mut self, id: Uuid, category: Uuid, name: &str, owner: Uuid, featured: bool) {
        use public_schema::items;
        diesel::insert_into(items::table)
            .values((
                items::id.eq(id),
                items::category_id.eq(Some(category)),
                items::name.eq(name),
                items::owner_id.eq(owner),
                items::is_featured.eq(featured),
            ))
            .execute(&mut self.connection)
            .expect("seed item on pg");
    }
}

impl PublicSqliteRunner {
    fn become_user(&mut self, user: Uuid) {
        self.acting = user;
        set_session_user_id(&user);
    }

    fn stored_count(&mut self, which: PublicStored) -> i64 {
        use public_schema::{categories_rls, items_rls};
        match which {
            PublicStored::Categories => {
                categories_rls::table
                    .count()
                    .get_result(&mut self.connection)
                    .expect("count categories_rls")
            }
            PublicStored::Items => {
                items_rls::table.count().get_result(&mut self.connection).expect("count items_rls")
            }
        }
    }

    fn seed_category(&mut self, id: Uuid, name: &str) {
        use public_schema::categories_rls;
        diesel::insert_into(categories_rls::table)
            .values((categories_rls::id.eq(id), categories_rls::name.eq(name)))
            .execute(&mut self.connection)
            .expect("seed categories_rls");
    }

    fn seed_item(&mut self, id: Uuid, category: Uuid, name: &str, owner: Uuid, featured: bool) {
        use public_schema::items_rls;
        diesel::insert_into(items_rls::table)
            .values((
                items_rls::id.eq(id),
                items_rls::category_id.eq(Some(category)),
                items_rls::name.eq(name),
                items_rls::owner_id.eq(owner),
                items_rls::is_featured.eq(featured),
            ))
            .execute(&mut self.connection)
            .expect("seed items_rls");
    }
}

fn public_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_session_user_role("authenticated")
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
}

fn public_access_scenario() -> Vec<PublicStep> {
    let cat2 = Uuid::from([0xc2; 16]);
    let item3 = Uuid::from([0x13; 16]);
    let item4 = Uuid::from([0x14; 16]);
    vec![
        PublicStep::Session(ua()),
        PublicStep::VisibleCategories, // ["tech"]
        PublicStep::InsertCategory { id: cat2, name: "science" }, // Accepted
        PublicStep::StoredCategories,  // 2
        PublicStep::InsertItem {
            id: item3,
            category: cat2,
            name: "alice item",
            owner: ua(),
            is_featured: false,
        }, // Accepted
        PublicStep::InsertItem {
            id: item4,
            category: cat2,
            name: "alice as bob item",
            owner: ub(),
            is_featured: false,
        }, // Refused
        PublicStep::StoredItems,       // 3
        PublicStep::Session(ub()),
        PublicStep::VisibleItems,      // ["featured widget"]
        PublicStep::VisibleCategories, // ["science", "tech"]
        PublicStep::StoredItems,       // 3 (unchanged)
    ]
}

#[test]
fn rls_public_access_engines_agree() {
    let cat1 = Uuid::from([0xc1; 16]);
    let item1 = Uuid::from([0x11; 16]);
    let item2 = Uuid::from([0x12; 16]);

    let mut pg = {
        let mut conn = postgres_harness::fresh_database();
        postgres_harness::apply(&mut conn, RLS_PUBLIC_ACCESS)
            .expect("apply rls_public_access to pg");
        // The TO authenticated INSERT policy requires this role to have DML.
        postgres_harness::apply(
            &mut conn,
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO authenticated",
        )
        .expect("grant DML to authenticated");
        PublicPgRunner { connection: conn, acting: ua(), current_role: String::new() }
    };
    let mut sq = {
        let translated = Pg2Sqlite::default()
            .sql(RLS_PUBLIC_ACCESS)
            .expect("parse rls_public_access")
            .translate(&public_options())
            .expect("translate rls_public_access");
        let mut conn = establish_connection();
        for stmt in &translated {
            diesel::sql_query(stmt.to_string())
                .execute(&mut conn)
                .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
        }
        PublicSqliteRunner { connection: conn, acting: ua() }
    };

    // Set a session user before seeds so the _rls audit triggers can query the
    // view.
    set_session_user_id(&ua());
    // Seed categories first; items reference them via FK.
    pg.seed_category(cat1, "tech");
    sq.seed_category(cat1, "tech");
    pg.seed_item(item1, cat1, "featured widget", ua(), true);
    sq.seed_item(item1, cat1, "featured widget", ua(), true);
    pg.seed_item(item2, cat1, "alice private", ua(), false);
    sq.seed_item(item2, cat1, "alice private", ua(), false);

    for step in public_access_scenario() {
        let expected = pg.step(&step);
        let actual = sq.step(&step);
        assert_eq!(actual, expected, "rls_public_access disagrees on {step:?}");
    }
}

// ════════════════════════════════════════════════════════════════════════
// rls_current_user: policies on current_user, role-name-based identity.
// ════════════════════════════════════════════════════════════════════════

const RLS_CURRENT_USER: &str = include_str!("../fixtures/rls_current_user.sql");

mod current_user_schema {
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        profiles (id) {
            id -> Uuid,
            username -> Text,
            email -> Text,
            is_public -> Bool,
        }
    }
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;
        /// Backing table for the profiles view on the SQLite side.
        profiles_rls (id) {
            id -> Uuid,
            username -> Text,
            email -> Text,
            is_public -> Bool,
        }
    }
}

#[derive(Debug)]
enum CurrentUserStep {
    /// PG: SET ROLE <name>. SQLite: set_session_username(<name>).
    RoleSession(&'static str),
    InsertProfile {
        id: Uuid,
        username: &'static str,
        email: &'static str,
        is_public: bool,
    },
    VisibleUsernames,
    StoredCount,
}

macro_rules! current_user_runner {
    ($name:ident, $conn:ty) => {
        struct $name {
            connection: $conn,
            current_role: String,
        }
        impl $name {
            fn step(&mut self, step: &CurrentUserStep) -> Outcome {
                use current_user_schema::profiles;
                match step {
                    CurrentUserStep::RoleSession(name) => {
                        self.set_role(name);
                        Outcome::Accepted
                    }
                    CurrentUserStep::InsertProfile { id, username, email, is_public } => {
                        Outcome::of(
                            &diesel::insert_into(profiles::table)
                                .values((
                                    profiles::id.eq(*id),
                                    profiles::username.eq(*username),
                                    profiles::email.eq(*email),
                                    profiles::is_public.eq(*is_public),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    CurrentUserStep::VisibleUsernames => {
                        let names: Vec<String> = profiles::table
                            .select(profiles::username)
                            .order(profiles::username.asc())
                            .load(&mut self.connection)
                            .expect("visible usernames");
                        Outcome::Rows(names)
                    }
                    CurrentUserStep::StoredCount => Outcome::Count(self.stored_count()),
                }
            }
        }
    };
}

current_user_runner!(CurrentUserPgRunner, PgConnection);
current_user_runner!(CurrentUserSqliteRunner, SqliteConnection);

impl CurrentUserPgRunner {
    fn set_role(&mut self, name: &str) {
        self.current_role = name.to_string();
        // Vendor-specific role command; current_user returns this name.
        diesel::sql_query(format!("SET ROLE {name}"))
            .execute(&mut self.connection)
            .expect("set role");
    }

    fn stored_count(&mut self) -> i64 {
        use current_user_schema::profiles;
        let role = self.current_role.clone();
        diesel::sql_query("RESET ROLE").execute(&mut self.connection).expect("reset role");
        let count =
            profiles::table.count().get_result(&mut self.connection).expect("count profiles");
        if !role.is_empty() {
            diesel::sql_query(format!("SET ROLE {role}"))
                .execute(&mut self.connection)
                .expect("restore role");
        }
        count
    }

    fn seed(&mut self, id: Uuid, username: &str, email: &str, is_public: bool) {
        use current_user_schema::profiles;
        // Running as superuser before any RoleSession step; no RLS applies.
        diesel::insert_into(profiles::table)
            .values((
                profiles::id.eq(id),
                profiles::username.eq(username),
                profiles::email.eq(email),
                profiles::is_public.eq(is_public),
            ))
            .execute(&mut self.connection)
            .expect("seed profile on pg");
    }
}

impl CurrentUserSqliteRunner {
    fn set_role(&mut self, name: &str) {
        self.current_role = name.to_string();
        set_session_username(name);
    }

    fn stored_count(&mut self) -> i64 {
        use current_user_schema::profiles_rls;
        profiles_rls::table.count().get_result(&mut self.connection).expect("count profiles_rls")
    }

    fn seed(&mut self, id: Uuid, username: &str, email: &str, is_public: bool) {
        use current_user_schema::profiles_rls;
        diesel::insert_into(profiles_rls::table)
            .values((
                profiles_rls::id.eq(id),
                profiles_rls::username.eq(username),
                profiles_rls::email.eq(email),
                profiles_rls::is_public.eq(is_public),
            ))
            .execute(&mut self.connection)
            .expect("seed profiles_rls");
    }
}

fn current_user_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_session_user_role("authenticated")
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::current_user("current_app_username"))
}

fn current_user_scenario() -> Vec<CurrentUserStep> {
    let prof2 = Uuid::from([0x22; 16]);
    let prof3 = Uuid::from([0x23; 16]);
    vec![
        CurrentUserStep::RoleSession("app"),
        // app has no profile yet; anon profile is public so app sees it.
        CurrentUserStep::VisibleUsernames, // ["anon"]
        CurrentUserStep::InsertProfile {
            id: prof2,
            username: "app",
            email: "app@test.com",
            is_public: false,
        }, // Accepted (username = current_user)
        CurrentUserStep::InsertProfile {
            id: prof3,
            username: "anon",
            email: "fake@test.com",
            is_public: false,
        }, // Refused (username != current_user)
        CurrentUserStep::StoredCount,      // 2
        CurrentUserStep::VisibleUsernames, // ["anon", "app"]
        CurrentUserStep::RoleSession("anon"),
        // anon sees own profile (public) but not app's private profile.
        CurrentUserStep::VisibleUsernames, // ["anon"]
    ]
}

#[test]
fn rls_current_user_engines_agree() {
    let prof1 = Uuid::from([0x21; 16]);

    let mut pg = {
        let mut conn = postgres_harness::fresh_database();
        postgres_harness::apply(&mut conn, RLS_CURRENT_USER).expect("apply rls_current_user to pg");
        // Both app and anon roles need DML grants since the scenario switches between
        // them.
        postgres_harness::apply(
            &mut conn,
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO app, anon",
        )
        .expect("grant DML to app and anon");
        CurrentUserPgRunner { connection: conn, current_role: String::new() }
    };
    let mut sq = {
        let translated = Pg2Sqlite::default()
            .sql(RLS_CURRENT_USER)
            .expect("parse rls_current_user")
            .translate(&current_user_options())
            .expect("translate rls_current_user");
        let mut conn = establish_connection();
        for stmt in &translated {
            diesel::sql_query(stmt.to_string())
                .execute(&mut conn)
                .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
        }
        CurrentUserSqliteRunner { connection: conn, current_role: String::new() }
    };

    // Set a session username so the profiles_rls audit trigger can query the view.
    set_session_username("anon");
    // Seed the anon profile (public) before any role is set on either engine.
    pg.seed(prof1, "anon", "anon@test.com", true);
    sq.seed(prof1, "anon", "anon@test.com", true);

    for step in current_user_scenario() {
        let expected = pg.step(&step);
        let actual = sq.step(&step);
        assert_eq!(actual, expected, "rls_current_user disagrees on {step:?}");
    }
}
