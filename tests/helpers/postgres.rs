//! A real PostgreSQL to check the suite's inputs against.
//!
//! Include in a test binary with:
//!
//! ```rust,ignore
//! #[path = "helpers/postgres.rs"]
//! mod postgres_harness;
//! ```
//!
//! Using `#[path]` rather than a sub-module of `helpers/mod.rs` keeps the
//! container machinery compiled into the binaries that ask for it, the same
//! reason `run_translated.rs` is included that way.
//!
//! The translator's guarantee is that SQLite accepts its output, which says
//! nothing about the input being valid PostgreSQL. A fixture PostgreSQL refuses
//! can therefore sit in the tree and stay green, which is what the permissions
//! fixture did for as long as it existed. These helpers make the other engine
//! available so that a test can say what PostgreSQL does rather than assume it.

#![allow(dead_code)]

use std::sync::{
    LazyLock, Mutex,
    atomic::{AtomicU32, Ordering},
};

use diesel::{connection::SimpleConnection, prelude::*, sql_types::BigInt};
use testcontainers::{Container, ImageExt, runners::SyncRunner};
use testcontainers_modules::postgres::Postgres;

/// Pinned rather than floating, so a new release cannot silently change what a
/// test measured.
pub const IMAGE_TAG: &str = "18-alpine";

/// What the fixtures assume a deployment has already provided.
///
/// The roles are named by `TO <role>` clauses in policies, and
/// `current_app_user` is the function this project's own documentation tells a
/// deployment to map `current_setting('app.user_id')` onto. `uuidv7` and
/// `uuidv4` are not here: they are `pg_catalog` functions from PostgreSQL 18,
/// which is the pin, so the server provides them.
pub const PRELUDE: &str = "
CREATE FUNCTION current_app_user() RETURNS uuid LANGUAGE sql STABLE
    AS $$ SELECT current_setting('app.user_id', true)::uuid $$;
CREATE FUNCTION current_app_username() RETURNS text LANGUAGE sql STABLE
    AS $$ SELECT current_setting('app.username', true) $$;
";

/// Roles the policies name, created once for the cluster rather than per
/// database, which is where they live.
const ROLES: [&str; 4] = ["app", "app_user", "authenticated", "anon"];

/// A session setting has to exist before a one-argument `current_setting` reads
/// it, and a policy that reads one errors otherwise. A deployment sets this per
/// connection, so the harness does too.
pub const DEFAULT_SESSION_USER: &str = "11111111-1111-1111-1111-111111111111";

/// The container lives as long as the test binary. One start per binary, since
/// starting it costs more than every case that uses it put together.
static SERVER: LazyLock<Server> = LazyLock::new(Server::start);

struct Server {
    /// Held only to keep the container alive. The mutex is what lets a handle
    /// that is merely `Send` live in a static the whole binary reads.
    _container: Mutex<Container<Postgres>>,
    port: u16,
    next_database: AtomicU32,
}

impl Server {
    fn start() -> Self {
        let container =
            Postgres::default().with_tag(IMAGE_TAG).start().expect("start a PostgreSQL container");
        let port = container.get_host_port_ipv4(5432).expect("the mapped port");

        let mut admin = connect(port, "postgres");
        for role in ROLES {
            // A role is cluster-wide, so this runs once and every database sees
            // it. `LOGIN` matters only for the ones a test connects as.
            admin
                .batch_execute(&format!("CREATE ROLE {role} LOGIN"))
                .unwrap_or_else(|error| panic!("create role {role}: {error}"));
        }

        Self { _container: Mutex::new(container), port, next_database: AtomicU32::new(0) }
    }
}

fn connect(port: u16, database: &str) -> PgConnection {
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/{database}");
    PgConnection::establish(&url).expect("connect to the container")
}

/// A database of its own, carrying [`PRELUDE`], so one case cannot leave
/// anything behind for the next.
///
/// # Panics
///
/// Panics when the container will not start, or the database cannot be made.
pub fn fresh_database() -> PgConnection {
    let server = &*SERVER;
    let ordinal = server.next_database.fetch_add(1, Ordering::Relaxed);
    let name = format!("case_{ordinal}");

    let mut admin = connect(server.port, "postgres");
    admin
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .unwrap_or_else(|error| panic!("create database {name}: {error}"));

    let mut connection = connect(server.port, &name);
    connection.batch_execute(PRELUDE).expect("apply the prelude");
    set_session_user(&mut connection, DEFAULT_SESSION_USER);
    connection
}

/// Applies a script through the simple query protocol, which is what a file of
/// several statements and dollar-quoted function bodies needs. The error is
/// returned rather than raised, since for most of these tests a refusal is the
/// measurement.
///
/// # Errors
///
/// Returns what PostgreSQL said.
pub fn apply(connection: &mut PgConnection, script: &str) -> Result<(), String> {
    connection.batch_execute(script).map_err(|error| error.to_string())
}

/// Sets the session variable the policies read.
///
/// # Panics
///
/// Panics when the setting cannot be applied.
pub fn set_session_user(connection: &mut PgConnection, user: &str) {
    // A session setting is not something the query DSL can express.
    diesel::sql_query(format!("SET app.user_id = '{user}'"))
        .execute(connection)
        .expect("set the session user");
    diesel::sql_query(format!("SET app.username = '{user}'"))
        .execute(connection)
        .expect("set the session username");
}

/// Reads every table once as a role that does not bypass row-level security.
///
/// This is the half a DDL check misses. A policy that consults another policy
/// applies cleanly and only fails when something reads it, so applying the
/// schema proves nothing on its own.
///
/// # Errors
///
/// Returns what PostgreSQL said about the first table it could not read.
pub fn read_every_table(connection: &mut PgConnection, role: &str) -> Result<(), String> {
    let probe = format!(
        "GRANT SELECT ON ALL TABLES IN SCHEMA public TO {role};
         SET ROLE {role};
         DO $$ DECLARE r record; n bigint; BEGIN
           FOR r IN SELECT tablename FROM pg_tables WHERE schemaname = 'public' LOOP
             EXECUTE format('SELECT count(*) FROM %I', r.tablename) INTO n;
           END LOOP;
         END $$;
         RESET ROLE;"
    );
    connection.batch_execute(&probe).map_err(|error| error.to_string())
}

/// Counts rows without a policy in the way, for asking what the storage holds
/// rather than what a user may see.
///
/// # Panics
///
/// Panics when the count will not run.
pub fn stored_count(connection: &mut PgConnection, table: &str) -> i64 {
    diesel::sql_query("RESET ROLE").execute(connection).expect("reset role");
    diesel::sql_query(format!("SELECT count(*) AS count FROM {table}"))
        .get_result::<CountRow>(connection)
        .expect("count the table")
        .count
}

#[derive(QueryableByName)]
pub struct CountRow {
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// What a step did, for a comparison between the engines.
///
/// A refusal compares as a refusal, since the two engines word their messages
/// differently and the wording is not the contract. Rows changed is not here on
/// purpose: SQLite does not count what an `INSTEAD OF` trigger writes, so every
/// accepted write through a view reports zero there while PostgreSQL reports
/// what it wrote. What compares is acceptance and what the storage holds after.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Accepted,
    Refused,
    Rows(Vec<String>),
    Count(i64),
}

impl Outcome {
    /// Reads a write's result as accepted or refused.
    pub fn of<T>(result: &QueryResult<T>) -> Self {
        match result {
            Ok(_) => Self::Accepted,
            Err(_) => Self::Refused,
        }
    }
}

/// The fixtures, with what each one needs before PostgreSQL will take it.
pub struct Fixture {
    /// File name under `tests/fixtures`.
    pub name: &'static str,
    /// Fixtures that have to be applied first, in order.
    pub requires: &'static [&'static str],
    /// Set when PostgreSQL cannot take this input at all, with the reason.
    pub skip: Option<&'static str>,
}

/// Every fixture in the tree. Measured against `postgres:18`, so the one
/// exclusion is a fact rather than a precaution.
pub const FIXTURES: &[Fixture] = &[
    Fixture { name: "data_types_extended.sql", requires: &[], skip: None },
    Fixture { name: "delete_using_rls.sql", requires: &[], skip: None },
    Fixture { name: "drop_statements.sql", requires: &[], skip: None },
    Fixture { name: "fts5_rls.sql", requires: &[], skip: None },
    Fixture { name: "grant_filtering.sql", requires: &[], skip: None },
    Fixture { name: "groups.sql", requires: &[], skip: None },
    Fixture {
        name: "missing_types.sql",
        requires: &[],
        skip: Some(
            "declares NVARCHAR, which PostgreSQL does not have. The fixture exists to exercise a \
             type alias in the translator, so it is not PostgreSQL source and never was.",
        ),
    },
    Fixture { name: "recursive_view.sql", requires: &[], skip: None },
    Fixture { name: "rls_all_policy.sql", requires: &[], skip: None },
    Fixture { name: "rls_basic.sql", requires: &[], skip: None },
    Fixture { name: "rls_current_user.sql", requires: &[], skip: None },
    Fixture { name: "rls_fk_both.sql", requires: &[], skip: None },
    Fixture { name: "rls_fk_simple.sql", requires: &[], skip: None },
    Fixture { name: "rls_grants.sql", requires: &["groups.sql"], skip: None },
    Fixture { name: "rls_multi_session_vars.sql", requires: &[], skip: None },
    Fixture { name: "rls_multiple_policies.sql", requires: &[], skip: None },
    Fixture { name: "rls_public_access.sql", requires: &[], skip: None },
    Fixture { name: "rls_self_referential.sql", requires: &[], skip: None },
    Fixture { name: "rls_tenant_isolation.sql", requires: &[], skip: None },
    Fixture { name: "trigger_elsif_else.sql", requires: &[], skip: None },
    Fixture { name: "trigger_issue.sql", requires: &[], skip: None },
    Fixture { name: "trigger_uuid_insert.sql", requires: &[], skip: None },
    Fixture { name: "trigger_with_recursive.sql", requires: &[], skip: None },
    Fixture {
        name: "vector_rls.sql",
        requires: &[],
        skip: Some(
            "declares a `vector` column, which needs the pgvector extension and so a different \
             image than the one this harness pins.",
        ),
    },
    Fixture { name: "views.sql", requires: &[], skip: None },
];

/// A fixture's text with whatever it composes on top of already prepended.
///
/// # Panics
///
/// Panics when a fixture names a file that is not there.
pub fn fixture_source(fixture: &Fixture) -> String {
    let read = |name: &str| {
        std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|error| panic!("read fixture {name}: {error}"))
    };
    let mut source = String::new();
    for required in fixture.requires {
        source.push_str(&read(required));
        source.push('\n');
    }
    source.push_str(&read(fixture.name));
    source
}

/// Additional setup for corpus-level tests, applied after the base prelude.
///
/// The base prelude omits `uuid_generate_v4` because the fixtures do not need
/// it, but the corpus exercises it directly.
pub const CORPUS_PRELUDE: &str = "
CREATE FUNCTION uuid_generate_v4() RETURNS uuid LANGUAGE sql VOLATILE AS $$ SELECT gen_random_uuid() $$;
";

/// Drops any role a fixture declares, so the fixture's own `CREATE ROLE` runs
/// for real rather than colliding with the harness.
///
/// A role lives at cluster scope while every case gets a database of its own,
/// so a name the harness pre-created, or an earlier case created, is still
/// there. Dropping it first is what keeps the fixture applied verbatim, which
/// matters when the point of the test is what PostgreSQL makes of the fixture
/// as written. A role carrying privileges from an earlier case cannot be
/// dropped, and that failure is deliberately left to surface rather than being
/// swallowed, since it would mean a fixture is being applied twice to one
/// cluster and the second run is no longer testing what it says.
///
/// # Panics
///
/// Panics when a declared role exists and cannot be dropped.
pub fn drop_declared_roles(connection: &mut PgConnection, source: &str) {
    for name in declared_roles(source) {
        diesel::sql_query(format!("DROP ROLE IF EXISTS {name}"))
            .execute(connection)
            .unwrap_or_else(|error| panic!("drop role {name} the fixture declares: {error}"));
    }
}

/// The role names a fixture declares, read off `CREATE ROLE <name>` and
/// limited to plain identifiers, which is every one the fixtures use.
fn declared_roles(source: &str) -> Vec<String> {
    source
        .match_indices("CREATE ROLE ")
        .map(|(at, keyword)| {
            source[at + keyword.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|name| !name.is_empty())
        .collect()
}
