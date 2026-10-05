//! The permissions fixture has to mean the same thing in both engines.
//!
//! `tests/fixtures/rls_grants.sql` is PostgreSQL source that the translator
//! turns into SQLite, and nothing else in the suite ever runs it on
//! PostgreSQL. For a long time it described a model PostgreSQL refuses: its
//! read policies consulted each other, and consulting a guarded table applies
//! that table's policy, so the consultations formed a loop that PostgreSQL
//! answers with `infinite recursion detected in policy for relation`. The
//! translated form ran anyway, because the translator rewrote every such
//! consultation to a backing table, which carries no policy and so has no next
//! policy to enter.
//!
//! This test drives one list of steps against both engines from the same
//! fixture text and asserts they agree, so the fixture cannot drift back into
//! describing something only one of them can run.
//!
//! A superuser and a table owner both bypass row-level security, so the
//! PostgreSQL side acts as a role holding nothing but DML grants.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::{establish_connection, set_session_user_id};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};
use postgres_harness::Outcome;
use rosetta_uuid::Uuid;

use crate::{helpers, postgres_harness};

const GROUPS: &str = include_str!("../fixtures/groups.sql");
const GRANTS: &str = include_str!("../fixtures/rls_grants.sql");

/// DML grant applied after the fixture so the app role can act on the tables.
const PG_ROLE_GRANT: &str =
    "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO app";

mod schema {
    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;

        ownables (id) {
            id -> Uuid,
            title -> Text,
            created_by -> Uuid,
        }
    }

    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;

        /// Backing table for ownables on the SQLite side.
        ownables_rls (id) {
            id -> Uuid,
            title -> Text,
            created_by -> Uuid,
        }
    }

    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;

        ownable_owners (ownable_id, owner_id) {
            ownable_id -> Uuid,
            owner_id -> Uuid,
        }
    }

    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;

        /// Backing table for ownable_owners on the SQLite side.
        ownable_owners_rls (ownable_id, owner_id) {
            ownable_id -> Uuid,
            owner_id -> Uuid,
        }
    }

    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;

        ownable_administrators (ownable_id, administrator_id) {
            ownable_id -> Uuid,
            administrator_id -> Uuid,
            granted_by -> Uuid,
        }
    }

    diesel::table! {
        use diesel::sql_types::*;
        use rosetta_uuid::diesel_impls::Uuid;

        grants (id) {
            id -> Uuid,
            ownable_id -> Uuid,
            grantee_id -> Uuid,
            grantor_id -> Uuid,
            role_id -> SmallInt,
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
}

use schema::{grants, ownable_administrators, ownable_owners, ownables, users};

fn alice() -> Uuid {
    stable_uuid(0x11)
}

fn bob() -> Uuid {
    stable_uuid(0x22)
}

fn carol() -> Uuid {
    stable_uuid(0x33)
}

fn thing() -> Uuid {
    stable_uuid(0xa1)
}

fn grant_id() -> Uuid {
    stable_uuid(0xb1)
}

fn stable_uuid(byte: u8) -> Uuid {
    Uuid::from([byte; 16])
}

/// One move in the scenario, worded so both engines can carry it out.
#[derive(Debug)]
enum Step {
    /// Who acts from here on.
    Session(Uuid),
    CreateOwnable {
        id: Uuid,
        title: &'static str,
        creator: Uuid,
    },
    AddOwner {
        ownable: Uuid,
        owner: Uuid,
    },
    AddAdmin {
        ownable: Uuid,
        admin: Uuid,
    },
    GrantViewer {
        id: Uuid,
        ownable: Uuid,
        grantee: Uuid,
    },
    /// The titles the acting user can see.
    VisibleTitles,
    DeleteOwnable {
        ownable: Uuid,
    },
    /// What the storage holds whoever asks, which is how a refusal is shown to
    /// have left the data alone.
    StoredOwnables,
    StoredOwners,
}

/// Which storage a ground-truth count asks about.
#[derive(Clone, Copy)]
enum Stored {
    Ownables,
    Owners,
}

/// The scenario both engines run, step by step.
fn scenario() -> Vec<Step> {
    vec![
        Step::Session(alice()),
        Step::CreateOwnable { id: thing(), title: "alice thing", creator: alice() },
        // The fixture's trigger records the creator as the first owner.
        Step::StoredOwners,
        Step::VisibleTitles,
        Step::Session(bob()),
        Step::VisibleTitles,
        // A stranger cannot make himself an owner.
        Step::AddOwner { ownable: thing(), owner: bob() },
        Step::StoredOwners,
        // An owner can appoint an administrator.
        Step::Session(alice()),
        Step::AddAdmin { ownable: thing(), admin: bob() },
        Step::Session(bob()),
        Step::VisibleTitles,
        // An administrator can grant, but cannot appoint an owner.
        Step::AddOwner { ownable: thing(), owner: carol() },
        Step::GrantViewer { id: grant_id(), ownable: thing(), grantee: carol() },
        Step::Session(carol()),
        Step::VisibleTitles,
        // A viewer deletes nothing, and the row survives.
        Step::DeleteOwnable { ownable: thing() },
        Step::StoredOwnables,
        // The owner deletes it, and the owner rows go with it.
        Step::Session(alice()),
        Step::DeleteOwnable { ownable: thing() },
        Step::StoredOwnables,
        Step::StoredOwners,
    ]
}

/// Writes the runner twice, once per connection type. The statements are the
/// same diesel either way, but each connection type needs its own copy, and
/// spelling out the backend bounds costs more than this does.
macro_rules! runner {
    ($name:ident, $conn:ty) => {
        struct $name {
            connection: $conn,
            acting: Uuid,
        }

        impl $name {
            fn seed_users(&mut self) {
                for (id, name) in [(alice(), "alice"), (bob(), "bob"), (carol(), "carol")] {
                    diesel::insert_into(users::table)
                        .values((users::id.eq(id), users::name.eq(name)))
                        .execute(&mut self.connection)
                        .expect("seed a user");
                }
            }

            fn step(&mut self, step: &Step) -> Outcome {
                match step {
                    Step::Session(user) => {
                        self.become_user(*user);
                        Outcome::Accepted
                    }
                    Step::CreateOwnable { id, title, creator } => {
                        Outcome::of(
                            &diesel::insert_into(ownables::table)
                                .values((
                                    ownables::id.eq(*id),
                                    ownables::title.eq(*title),
                                    ownables::created_by.eq(*creator),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::AddOwner { ownable, owner } => {
                        Outcome::of(
                            &diesel::insert_into(ownable_owners::table)
                                .values((
                                    ownable_owners::ownable_id.eq(*ownable),
                                    ownable_owners::owner_id.eq(*owner),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::AddAdmin { ownable, admin } => {
                        let granted_by = self.acting;
                        Outcome::of(
                            &diesel::insert_into(ownable_administrators::table)
                                .values((
                                    ownable_administrators::ownable_id.eq(*ownable),
                                    ownable_administrators::administrator_id.eq(*admin),
                                    ownable_administrators::granted_by.eq(granted_by),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::GrantViewer { id, ownable, grantee } => {
                        let grantor = self.acting;
                        Outcome::of(
                            &diesel::insert_into(grants::table)
                                .values((
                                    grants::id.eq(*id),
                                    grants::ownable_id.eq(*ownable),
                                    grants::grantee_id.eq(*grantee),
                                    grants::grantor_id.eq(grantor),
                                    grants::role_id.eq(2_i16),
                                ))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::VisibleTitles => {
                        let mut titles: Vec<String> = ownables::table
                            .select(ownables::title)
                            .load(&mut self.connection)
                            .expect("a read through a policy must not error");
                        titles.sort();
                        Outcome::Rows(titles)
                    }
                    Step::DeleteOwnable { ownable } => {
                        Outcome::of(
                            &diesel::delete(ownables::table.filter(ownables::id.eq(*ownable)))
                                .execute(&mut self.connection),
                        )
                    }
                    Step::StoredOwnables => Outcome::Count(self.stored_count(Stored::Ownables)),
                    Step::StoredOwners => Outcome::Count(self.stored_count(Stored::Owners)),
                }
            }
        }
    };
}

runner!(PgRunner, PgConnection);
runner!(SqliteRunner, SqliteConnection);

impl PgRunner {
    fn become_user(&mut self, user: Uuid) {
        self.acting = user;
        // Vendor-specific session command, not expressible via Diesel DSL.
        diesel::sql_query(format!("SET app.user_id = '{user}'"))
            .execute(&mut self.connection)
            .expect("set session user");
    }

    fn stored_count(&mut self, stored: Stored) -> i64 {
        use schema::{ownable_owners, ownables};
        // Vendor-specific role command to read past RLS.
        diesel::sql_query("RESET ROLE").execute(&mut self.connection).expect("reset role");
        let count = match stored {
            Stored::Ownables => ownables::table.count().get_result(&mut self.connection),
            Stored::Owners => ownable_owners::table.count().get_result(&mut self.connection),
        }
        .expect("count the table");
        // Vendor-specific role restoration.
        diesel::sql_query("SET ROLE app").execute(&mut self.connection).expect("restore role");
        count
    }
}

impl SqliteRunner {
    fn become_user(&mut self, user: Uuid) {
        self.acting = user;
        set_session_user_id(&user);
    }

    fn stored_count(&mut self, stored: Stored) -> i64 {
        use schema::{ownable_owners_rls, ownables_rls};
        match stored {
            Stored::Ownables => ownables_rls::table.count().get_result(&mut self.connection),
            Stored::Owners => ownable_owners_rls::table.count().get_result(&mut self.connection),
        }
        .expect("count the backing table")
    }
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_rls_audit_table_name("rls_audit")
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
}

fn sqlite_runner() -> SqliteRunner {
    let translated = Pg2Sqlite::default()
        .sql(GROUPS)
        .expect("parse the groups fixture")
        .sql(GRANTS)
        .expect("parse the permissions fixture")
        .translate(&options())
        .expect("translate");

    let mut connection = establish_connection();
    for statement in &translated {
        // DDL migration: raw SQL is the correct form here.
        diesel::sql_query(statement.to_string())
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted DDL failed: {error}\n{statement}"));
    }

    let mut runner = SqliteRunner { connection, acting: alice() };
    runner.become_user(alice());
    runner.seed_users();
    runner
}

fn pg_runner() -> PgRunner {
    let mut connection = postgres_harness::fresh_database();
    for script in [GROUPS, GRANTS] {
        postgres_harness::apply(&mut connection, script)
            .unwrap_or_else(|e| panic!("apply fixture: {e}"));
    }
    // The app role exists cluster-wide; this grants DML on the fixture tables.
    postgres_harness::apply(&mut connection, PG_ROLE_GRANT).expect("grant DML to app");

    let mut runner = PgRunner { connection, acting: alice() };
    runner.seed_users();
    // Vendor-specific role switch so RLS applies to subsequent queries.
    diesel::sql_query("SET ROLE app").execute(&mut runner.connection).expect("set role");
    runner.become_user(alice());
    runner
}

/// The fixture is PostgreSQL source, so what it means is what PostgreSQL says
/// it means. Every step is compared, and the first disagreement names itself.
#[test]
fn the_two_engines_agree_on_the_permissions_model() {
    let mut postgres = pg_runner();
    let mut sqlite = sqlite_runner();

    for step in scenario() {
        let expected = postgres.step(&step);
        let actual = sqlite.step(&step);
        assert_eq!(actual, expected, "the engines disagree on {step:?}");
    }
}
