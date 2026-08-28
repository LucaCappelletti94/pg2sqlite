//! When a policy omits `WITH CHECK`, PostgreSQL uses its `USING` expression as
//! the check on the new row.
//!
//! From the PostgreSQL `CREATE POLICY` documentation: "If no WITH CHECK
//! expression is defined, then the USING expression will be used both to
//! determine which rows are visible (normal USING case) and which new rows will
//! be allowed to be added (WITH CHECK case)."
//!
//! The practical consequence is that an UPDATE which moves a row OUT of the
//! policy predicate must be rejected. The translator used to read only
//! `check_expression()`, so a `FOR UPDATE USING (p)` policy produced an UPDATE
//! trigger with no guard at all: the write succeeded and the row silently
//! vanished from the view.
//!
//! The two `explicit_with_check_*` tests guard the other direction, that an
//! explicit `WITH CHECK` still wins over `USING` rather than being ANDed with
//! it.

mod helpers;

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        /// RLS view over `docs_rls`.
        docs (id) {
            id -> Integer,
            is_public -> Integer,
            body -> Text,
        }
    }

    diesel::table! {
        /// Backing table, read directly so assertions see what was stored.
        docs_rls (id) {
            id -> Integer,
            is_public -> Integer,
            body -> Text,
        }
    }
}

use schema::{docs, docs_rls};

#[derive(Insertable)]
#[diesel(table_name = docs_rls)]
struct BackingRow {
    id: i32,
    is_public: i32,
    body: String,
}

#[derive(Insertable)]
#[diesel(table_name = docs)]
struct ViewRow {
    id: i32,
    is_public: i32,
    body: String,
}

#[derive(Queryable, Selectable, Debug, PartialEq, Eq)]
#[diesel(table_name = docs_rls)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct StoredRow {
    id: i32,
    is_public: i32,
    body: String,
}

const TABLE: &str = "
    CREATE TABLE docs (
        id INTEGER PRIMARY KEY,
        is_public INTEGER NOT NULL,
        body TEXT NOT NULL
    );
    ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
";

/// Applies the translated DDL, then seeds one visible row. The emitted SQL is
/// the artifact under test so it is applied as generated text; all other
/// statements use the typed DSL. Triggers are applied after the seed because
/// the seed plays the system-load role, which runs with triggers disabled
/// under the authoritative-load contract, and diesel cannot reach
/// sqlite3_db_config.
fn setup(policies: &str) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let pg = format!("{TABLE}{policies}");
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let translated = Pg2Sqlite::default().sql(&pg)?.translate(&options)?;

    let mut conn = SqliteConnection::establish(":memory:")?;
    let (triggers, base): (Vec<_>, Vec<_>) =
        translated.iter().map(ToString::to_string).partition(|s| s.starts_with("CREATE TRIGGER"));
    for statement in &base {
        diesel::sql_query(statement.clone()).execute(&mut conn)?;
    }

    diesel::insert_into(docs_rls::table)
        .values(BackingRow { id: 1, is_public: 1, body: "original".to_owned() })
        .execute(&mut conn)?;

    for statement in &triggers {
        diesel::sql_query(statement.clone()).execute(&mut conn)?;
    }

    Ok(conn)
}

fn stored(conn: &mut SqliteConnection) -> Result<StoredRow, Box<dyn std::error::Error>> {
    Ok(docs_rls::table.filter(docs_rls::id.eq(1)).select(StoredRow::as_select()).first(conn)?)
}

/// `FOR UPDATE USING (p)` with no `WITH CHECK`: moving the row out of `p` must
/// be rejected, not silently accepted.
#[test]
fn for_update_using_rejects_moving_a_row_out_of_the_predicate()
-> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup(
        "
        CREATE POLICY sel ON docs FOR SELECT USING (is_public = 1);
        CREATE POLICY upd ON docs FOR UPDATE USING (is_public = 1);
    ",
    )?;

    let rejected = diesel::update(docs::table.filter(docs::id.eq(1)))
        .set(docs::is_public.eq(0))
        .execute(&mut conn);

    assert!(
        rejected.is_err(),
        "the USING expression doubles as WITH CHECK, so the new row must satisfy it"
    );
    assert_eq!(
        stored(&mut conn)?,
        StoredRow { id: 1, is_public: 1, body: "original".to_owned() },
        "the rejected update must leave the row untouched"
    );

    Ok(())
}

/// `FOR ALL USING (p)` with no `WITH CHECK`: same rule.
#[test]
fn for_all_using_rejects_moving_a_row_out_of_the_predicate()
-> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup("CREATE POLICY every ON docs FOR ALL USING (is_public = 1);")?;

    let rejected = diesel::update(docs::table.filter(docs::id.eq(1)))
        .set(docs::is_public.eq(0))
        .execute(&mut conn);

    assert!(rejected.is_err(), "a FOR ALL policy's USING also constrains the new row");
    assert_eq!(stored(&mut conn)?.is_public, 1);

    Ok(())
}

/// An update that keeps the row inside the predicate still succeeds. Guards
/// against the fallback rejecting everything.
#[test]
fn update_staying_inside_the_predicate_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup(
        "
        CREATE POLICY sel ON docs FOR SELECT USING (is_public = 1);
        CREATE POLICY upd ON docs FOR UPDATE USING (is_public = 1);
    ",
    )?;

    diesel::update(docs::table.filter(docs::id.eq(1)))
        .set(docs::body.eq("edited"))
        .execute(&mut conn)?;

    assert_eq!(stored(&mut conn)?, StoredRow { id: 1, is_public: 1, body: "edited".to_owned() });

    Ok(())
}

/// An explicit `WITH CHECK` replaces `USING` for the new row rather than being
/// combined with it, so a check laxer than the USING clause is honoured.
#[test]
fn explicit_with_check_replaces_using_for_the_new_row() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup(
        "
        CREATE POLICY sel ON docs FOR SELECT USING (is_public = 1);
        CREATE POLICY upd ON docs FOR UPDATE USING (is_public = 1) WITH CHECK (true);
    ",
    )?;

    // USING gates which row may be touched (it is visible), WITH CHECK (true)
    // permits the new value even though it leaves the USING predicate.
    diesel::update(docs::table.filter(docs::id.eq(1)))
        .set(docs::is_public.eq(0))
        .execute(&mut conn)?;

    assert_eq!(stored(&mut conn)?.is_public, 0, "an explicit WITH CHECK must win over USING");

    Ok(())
}

/// An explicit `WITH CHECK` stricter than `USING` is enforced.
#[test]
fn explicit_with_check_stricter_than_using_is_enforced() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup(
        "
        CREATE POLICY sel ON docs FOR SELECT USING (is_public = 1);
        CREATE POLICY upd ON docs FOR UPDATE USING (is_public = 1) WITH CHECK (body = 'allowed');
    ",
    )?;

    let rejected = diesel::update(docs::table.filter(docs::id.eq(1)))
        .set(docs::body.eq("denied"))
        .execute(&mut conn);
    assert!(rejected.is_err(), "the explicit WITH CHECK must reject this body");

    diesel::update(docs::table.filter(docs::id.eq(1)))
        .set(docs::body.eq("allowed"))
        .execute(&mut conn)?;
    assert_eq!(stored(&mut conn)?.body, "allowed");

    Ok(())
}

/// A `FOR SELECT` policy carries no write semantics, so its `USING` must not
/// leak into the UPDATE trigger's check on the new row. Only the UPDATE
/// policy's predicate governs the write.
#[test]
fn select_policy_using_does_not_constrain_updates() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup(
        "
        CREATE POLICY sel ON docs FOR SELECT USING (body <> 'hidden');
        CREATE POLICY upd ON docs FOR UPDATE USING (is_public = 1) WITH CHECK (is_public = 1);
    ",
    )?;

    // Sets body to 'hidden', which the SELECT policy would exclude, but the
    // UPDATE policy's own predicate is satisfied so the write is allowed.
    diesel::update(docs::table.filter(docs::id.eq(1)))
        .set(docs::body.eq("hidden"))
        .execute(&mut conn)?;

    assert_eq!(stored(&mut conn)?.body, "hidden");

    Ok(())
}

/// `FOR ALL USING (p)` with no `WITH CHECK` must also constrain an INSERT: the
/// USING expression doubles as the check on the new row. This is the INSERT
/// half of the same rule the UPDATE tests above cover.
#[test]
fn for_all_using_rejects_an_insert_violating_the_predicate()
-> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup("CREATE POLICY every ON docs FOR ALL USING (is_public = 1);")?;

    let rejected = diesel::insert_into(docs::table)
        .values(ViewRow { id: 2, is_public: 0, body: "private".to_owned() })
        .execute(&mut conn);

    assert!(rejected.is_err(), "the USING expression also gates the inserted row");
    assert_eq!(
        docs_rls::table.count().get_result::<i64>(&mut conn)?,
        1,
        "only the seeded row remains, the rejected insert stored nothing"
    );

    Ok(())
}

/// The same policy accepts an INSERT that satisfies the predicate.
#[test]
fn for_all_using_accepts_an_insert_satisfying_the_predicate()
-> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup("CREATE POLICY every ON docs FOR ALL USING (is_public = 1);")?;

    diesel::insert_into(docs::table)
        .values(ViewRow { id: 2, is_public: 1, body: "public".to_owned() })
        .execute(&mut conn)?;

    assert_eq!(docs_rls::table.count().get_result::<i64>(&mut conn)?, 2);

    Ok(())
}

/// An explicit `FOR INSERT WITH CHECK` is unaffected by the fallback.
#[test]
fn explicit_insert_with_check_is_enforced() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup(
        "
        CREATE POLICY sel ON docs FOR SELECT USING (true);
        CREATE POLICY ins ON docs FOR INSERT WITH CHECK (is_public = 1);
    ",
    )?;

    let rejected = diesel::insert_into(docs::table)
        .values(ViewRow { id: 2, is_public: 0, body: "private".to_owned() })
        .execute(&mut conn);
    assert!(rejected.is_err());

    diesel::insert_into(docs::table)
        .values(ViewRow { id: 3, is_public: 1, body: "public".to_owned() })
        .execute(&mut conn)?;
    assert_eq!(docs_rls::table.count().get_result::<i64>(&mut conn)?, 2);

    Ok(())
}

/// A `FOR INSERT` policy carrying neither `WITH CHECK` nor `USING` has no
/// predicate to apply, so it grants the insert.
///
/// PostgreSQL forbids `USING` on an INSERT-only policy, so the documented
/// fallback chain has nothing to fall back to and the policy imposes no
/// restriction. Characterization test: it pins that a missing predicate reads
/// as permissive-true rather than as a denial.
#[test]
fn insert_policy_without_any_predicate_grants_the_insert() -> Result<(), Box<dyn std::error::Error>>
{
    let mut conn = setup(
        "
        CREATE POLICY sel ON docs FOR SELECT USING (true);
        CREATE POLICY ins ON docs FOR INSERT;
    ",
    )?;

    diesel::insert_into(docs::table)
        .values(ViewRow { id: 2, is_public: 0, body: "anything".to_owned() })
        .execute(&mut conn)?;

    assert_eq!(docs_rls::table.count().get_result::<i64>(&mut conn)?, 2);

    Ok(())
}
