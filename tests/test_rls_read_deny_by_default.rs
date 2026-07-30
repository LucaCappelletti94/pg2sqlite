//! Read-path deny-by-default: an RLS view with no applicable SELECT policy must
//! expose no rows.
//!
//! Sibling of `test_rls_deny_by_default.rs`, which covers the WRITE path
//! (INSERT, UPDATE, DELETE without a matching policy). This file covers reads.
//!
//! PostgreSQL treats `ENABLE ROW LEVEL SECURITY` plus no applicable policy as a
//! permanent FALSE: the table owner sees rows, everyone else sees none. The
//! generated view used to carry no `WHERE` clause in that configuration, so it
//! exposed the whole backing table. The write path already denied correctly,
//! which made the read path an inconsistency rather than a design choice.
//!
//! A policy that omits `USING` is a different case and must NOT deny.
//! PostgreSQL treats a missing `USING` as permissive-true, so such a policy
//! grants every row. `policy_without_using_clause_grants_every_row` pins that
//! distinction so the deny-all fix cannot be over-applied.
//!
//! Denying every row also removes the point of the runtime validation monitor
//! for such a table: its check asks whether a backing row is visible through
//! the view, which for a deny-all view is always no, so it would fire on every
//! write and report nothing. That configuration is reported once at translation
//! time as a `TranslationWarning::RlsDeniesEveryRow` instead, and no monitoring
//! triggers are emitted.

mod helpers;

use diesel::prelude::*;
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
    warnings::TranslationWarning,
};

mod schema {
    diesel::table! {
        /// RLS view over `docs_rls`.
        docs (id) {
            id -> Integer,
            owner_id -> Integer,
        }
    }

    diesel::table! {
        /// Backing table, written directly to simulate a server-side sync that
        /// bypasses the view.
        docs_rls (id) {
            id -> Integer,
            owner_id -> Integer,
        }
    }

    diesel::table! {
        /// Audit table the RLS validation monitor writes to.
        rls_audit (id) {
            id -> Integer,
            table_name -> Text,
            violation_type -> Text,
            row_identifier -> Text,
            policy_name -> Nullable<Text>,
            detected_at -> Text,
            severity -> Text,
            details -> Nullable<Text>,
            reported_at -> Nullable<Text>,
        }
    }
}

use schema::{docs, docs_rls, rls_audit};

#[derive(Insertable)]
#[diesel(table_name = docs_rls)]
struct BackingRow {
    id: i32,
    owner_id: i32,
}

/// Translates `pg` with default options, applies the emitted DDL, then seeds
/// the backing table with two rows. Tests needing other options call [`apply`]
/// and [`seed`] directly, which also lets them assert that the seed was
/// rejected.
fn setup(pg: &str) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let mut conn = apply(pg, &options)?;
    seed(&mut conn)?;
    Ok(conn)
}

fn apply(
    pg: &str,
    options: &Pg2SqliteOptions,
) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let translated = Pg2Sqlite::default().sql(pg)?.translate(options)?;
    let mut conn = SqliteConnection::establish(":memory:")?;
    for statement in &translated {
        diesel::sql_query(statement.to_string()).execute(&mut conn)?;
    }
    Ok(conn)
}

fn seed(conn: &mut SqliteConnection) -> diesel::QueryResult<usize> {
    diesel::insert_into(docs_rls::table)
        .values(vec![BackingRow { id: 1, owner_id: 7 }, BackingRow { id: 2, owner_id: 9 }])
        .execute(conn)
}

fn view_count(conn: &mut SqliteConnection) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(docs::table.count().get_result(conn)?)
}

fn backing_count(conn: &mut SqliteConnection) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(docs_rls::table.count().get_result(conn)?)
}

/// RLS enabled, zero policies declared: the view must expose nothing.
#[test]
fn rls_enabled_with_no_policies_denies_every_row() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup(
        "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
    ",
    )?;

    assert_eq!(backing_count(&mut conn)?, 2, "backing table should hold the seeded rows");
    assert_eq!(
        view_count(&mut conn)?,
        0,
        "RLS enabled with no policy denies every row in PostgreSQL, so the view must be empty"
    );

    Ok(())
}

/// Policies exist but none applies to SELECT: still nothing visible.
#[test]
fn rls_with_only_non_select_policies_denies_every_row() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup(
        "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_insert ON docs FOR INSERT WITH CHECK (owner_id > 0);
        CREATE POLICY docs_delete ON docs FOR DELETE USING (owner_id > 0);
    ",
    )?;

    assert_eq!(backing_count(&mut conn)?, 2);
    assert_eq!(view_count(&mut conn)?, 0, "no SELECT or ALL policy applies, so no row is readable");

    Ok(())
}

/// A SELECT policy with a real predicate still filters normally. Guards against
/// the deny-all path swallowing the ordinary case.
#[test]
fn rls_with_select_policy_filters_normally() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup(
        "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id = 7);
    ",
    )?;

    assert_eq!(view_count(&mut conn)?, 1, "only the owner_id = 7 row is visible");

    Ok(())
}

/// A policy that omits `USING` is permissive-true in PostgreSQL, so it grants
/// every row. The deny-all fix must not treat a missing predicate as a denial.
#[test]
fn policy_without_using_clause_grants_every_row() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup(
        "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_select ON docs FOR SELECT;
    ",
    )?;

    assert_eq!(
        view_count(&mut conn)?,
        2,
        "a SELECT policy with no USING clause is permissive-true, so every row is visible"
    );

    Ok(())
}

/// A `FOR ALL` policy applies to SELECT, so such a table filters normally and
/// must NOT reach the deny-all path.
///
/// Guards a latent regression whose severity this change raised: if
/// `filter_policies` ever stopped treating `CreatePolicyCommand::All` as
/// applying to SELECT, the old failure mode was an unfiltered view (rows still
/// visible) and the new one is denying everything.
#[test]
fn rls_with_only_a_for_all_policy_filters_normally() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup(
        "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_all ON docs FOR ALL USING (owner_id = 7);
    ",
    )?;

    assert_eq!(
        view_count(&mut conn)?,
        1,
        "a FOR ALL policy applies to SELECT, so it filters rather than denying"
    );

    Ok(())
}

/// A deny-all table gets no runtime monitor at all, so a backing-table write
/// logs nothing.
///
/// The monitor exists to catch a row in the backing table that the policies
/// would hide, which is a discrepancy between data and policy. With no policy
/// there is nothing to disagree with: the check would fire on every write and
/// so carry no information. The condition is reported once at translation time
/// instead, which `deny_all_table_warns_once_at_translation_time` covers.
#[test]
fn deny_all_view_is_not_monitored_at_runtime() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup(
        "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
    ",
    )?;

    let logged: i64 = rls_audit::table.count().get_result(&mut conn)?;
    assert_eq!(logged, 0, "a monitor that could only ever fire is not emitted");

    Ok(())
}

/// Strict validation no longer rejects a backing write to a deny-all table.
///
/// "RLS enabled, policies arrive in a later migration" is an ordinary
/// mid-migration state, and failing every write in it blocked a data load for a
/// configuration problem that translation already reports.
#[test]
fn strict_validation_over_a_deny_all_view_accepts_backing_writes()
-> Result<(), Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_strict_rls_validation();
    let mut conn = apply(
        "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
    ",
        &options,
    )?;

    seed(&mut conn)?;
    assert_eq!(
        docs_rls::table.count().get_result::<i64>(&mut conn)?,
        2,
        "both rows must reach the backing table"
    );
    assert_eq!(
        docs::table.count().get_result::<i64>(&mut conn)?,
        0,
        "they still must not be readable through the deny-all view"
    );

    Ok(())
}

/// The deny-all configuration is reported once, at translation time, rather
/// than once per row at runtime.
#[test]
fn deny_all_table_warns_once_at_translation_time() -> Result<(), Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let report = Pg2Sqlite::default()
        .sql(
            "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
    ",
        )?
        .translate_with_report(&options)?;

    let denials: Vec<_> = report
        .warnings
        .iter()
        .filter(|warning| {
            matches!(
                warning,
                TranslationWarning::RlsDeniesEveryRow { table } if table == "docs"
            )
        })
        .collect();

    assert_eq!(denials.len(), 1, "exactly one warning per table, got {:?}", report.warnings);

    Ok(())
}

/// A table with a working policy is still monitored, and still discriminates.
///
/// The seed writes `owner_id` 7 and 9, and the policy admits only 7, so exactly
/// one row is invisible. Asserting one rather than two proves the monitor is
/// still evaluating the predicate, not merely firing, which is what makes the
/// deny-all skip a scoped change instead of a way to switch validation off.
#[test]
fn a_table_with_a_policy_is_still_monitored() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup(
        "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY p ON docs FOR SELECT USING (owner_id = 7);
    ",
    )?;

    let logged: i64 = rls_audit::table.count().get_result(&mut conn)?;
    assert_eq!(logged, 1, "the row invisible under the policy is still reported");

    Ok(())
}
