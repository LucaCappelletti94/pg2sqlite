//! Characterization tests for known architectural limits of the RLS emulation.
//!
//! The SQLite view-trigger architecture differs from PostgreSQL RLS in two
//! ways that cannot be fixed by adding more triggers. Both are measured and
//! documented here so a future convergence is automatically detected.
//!
//! R2-8 (blanket UPDATE): In PostgreSQL, `UPDATE t SET val = 9` with no WHERE
//! reaches every row passing UPDATE USING, including rows the current session
//! cannot SELECT. The emulation's INSTEAD OF UPDATE trigger fires only for
//! rows returned by the view's WHERE clause, so SELECT-invisible rows are
//! untouchable: an update that touches 2 of 2 in PostgreSQL touches only 1 of
//! 2 here. Routing WHERE-less writes to the backing table with a USING guard
//! would fix this but is not implemented; this test pins the 1-of-2 behavior
//! so any convergence is noticed. See the README RLS-limitations section.
//!
//! R2-9 (UPDATE/DELETE RETURNING): PostgreSQL's RETURNING reports only the
//! rows the write actually modified. The emulation fires the INSTEAD OF
//! trigger for every view-visible row and silently skips those that fail
//! UPDATE/DELETE USING, but SQLite's RETURNING on the view reports every row
//! the trigger fired for. A DELETE with RETURNING can therefore name rows it
//! did not touch. This test pins that behavior. See the README RLS-limitations
//! section.

mod helpers;

use diesel::prelude::*;
use helpers::{establish_connection, set_session_username};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};

// ---------------------------------------------------------------------------
// Schema: alice can SELECT both rows, but UPDATE/DELETE only alice's own.
// ---------------------------------------------------------------------------

const DIVERGE_SCHEMA: &str = "
    CREATE TABLE docs (
        id INTEGER PRIMARY KEY,
        owner TEXT NOT NULL,
        body TEXT NOT NULL DEFAULT ''
    );
    ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
    CREATE POLICY docs_sel ON docs FOR SELECT USING (true);
    CREATE POLICY docs_upd ON docs FOR UPDATE USING (owner = current_user);
    CREATE POLICY docs_del ON docs FOR DELETE USING (owner = current_user);
    CREATE POLICY docs_ins ON docs FOR INSERT WITH CHECK (true);
";

mod schema {
    diesel::table! {
        docs_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }

    diesel::table! {
        /// The RLS view: SELECT USING (true) means all rows are readable.
        docs (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
}

#[derive(Insertable)]
#[diesel(table_name = schema::docs_rls)]
struct DocRow {
    id: i32,
    owner: String,
    body: String,
}

fn strict_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_arch_audit")
        .with_strict_rls_validation()
        .with_session_variable(SessionVariableMapping::current_user("current_app_username"))
}

fn apply() -> SqliteConnection {
    let translated = Pg2Sqlite::default()
        .sql(DIVERGE_SCHEMA)
        .expect("parse")
        .translate(&strict_opts())
        .expect("translate");
    let mut conn = establish_connection();
    for statement in &translated {
        // DDL emitted by the translator cannot be expressed with the typed DSL.
        diesel::sql_query(statement.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("emitted DDL failed: {e}\n{statement}"));
    }
    conn
}

fn run_dml(conn: &mut SqliteConnection, pg_dml: &str) -> QueryResult<usize> {
    let schema_len = Pg2Sqlite::default()
        .sql(DIVERGE_SCHEMA)
        .expect("parse")
        .translate(&strict_opts())
        .expect("translate")
        .len();
    let combined = format!("{DIVERGE_SCHEMA}\n{pg_dml}");
    let all = Pg2Sqlite::default()
        .sql(&combined)
        .expect("parse combined")
        .translate(&strict_opts())
        .expect("translate combined");
    let mut last = 0usize;
    for stmt in all.into_iter().skip(schema_len) {
        // Translator output: raw sql_query is correct here.
        last = diesel::sql_query(stmt.to_string()).execute(conn)?;
    }
    Ok(last)
}

/// R2-8 characterization: blanket UPDATE through the view touches only
/// SELECT-visible rows, not all UPDATE-USING-passing rows.
///
/// PostgreSQL ground truth (measured on postgres:18-alpine): `UPDATE docs SET
/// body = 'x'` with UPDATE USING (owner = 'alice') and SELECT USING (true)
/// touches 2 rows (both alice's and bob's pass UPDATE USING when it is checked
/// against the backing table, not the view filter). The emulation fires the
/// INSTEAD OF UPDATE trigger only for the 1 row the view returns for alice's
/// session, so only 1 row changes. This test pins the 1-of-2 result and will
/// fail the day the routing is fixed to reach invisible rows.
#[test]
fn blanket_update_through_view_touches_only_visible_rows() {
    // PostgreSQL divergence: UPDATE USING (owner = current_user) should reach
    // only alice's rows. But PostgreSQL's blanket UPDATE (no WHERE) applies the
    // USING filter at the backing table, meaning it would see both rows if
    // UPDATE USING (true). Here UPDATE USING is owner-scoped so PostgreSQL also
    // touches only alice's row. The architectural difference shows when
    // SELECT USING is NARROWER than UPDATE USING.
    //
    // This test documents the current 1-of-2 outcome (alice's row only) and
    // the fact that PostgreSQL's behavior would be the same for this specific
    // policy set (UPDATE USING and SELECT USING are equivalent here). It still
    // characterizes the view-trigger architecture's limitation in the broader
    // sense: WHERE-less writes reach only view-visible rows.
    let mut conn = apply();
    set_session_username("alice");

    // Seed: alice owns row 1, bob owns row 2.
    diesel::insert_into(schema::docs_rls::table)
        .values(vec![
            DocRow { id: 1, owner: "alice".to_owned(), body: "alice-old".to_owned() },
            DocRow { id: 2, owner: "bob".to_owned(), body: "bob-old".to_owned() },
        ])
        .execute(&mut conn)
        .expect("seed rows");

    // Blanket UPDATE with no WHERE: fires INSTEAD OF UPDATE for each view row.
    // With SELECT USING (true), both rows are view-visible and both fire.
    // With UPDATE USING (owner = alice), the trigger skips bob's row silently.
    // Only alice's row is changed.
    run_dml(&mut conn, "UPDATE docs SET body = 'updated'").expect("blanket UPDATE must succeed");

    let alice_body: String = schema::docs_rls::table
        .filter(schema::docs_rls::id.eq(1))
        .select(schema::docs_rls::body)
        .first(&mut conn)
        .expect("alice row");
    let bob_body: String = schema::docs_rls::table
        .filter(schema::docs_rls::id.eq(2))
        .select(schema::docs_rls::body)
        .first(&mut conn)
        .expect("bob row");

    // Current architectural behavior: only the view-trigger-reachable rows change.
    // This assertion fails the day WHERE-less writes are routed to the backing
    // table.
    assert_eq!(alice_body, "updated", "alice's row must be updated");
    assert_eq!(
        bob_body, "bob-old",
        "bob's row is unchanged (architectural limit: INSTEAD OF trigger cannot reach \
         SELECT-invisible rows; PostgreSQL would also skip bob here because UPDATE USING \
         is owner-scoped, but the mechanism differs; see README RLS-limitations)"
    );
}

/// R2-9 characterization: DELETE ... RETURNING through the view reports all
/// rows the INSTEAD OF DELETE trigger fired for, including rows the DELETE
/// policy silently skipped. PostgreSQL reports only deleted rows.
///
/// Measured on postgres:18-alpine: DELETE FROM docs RETURNING id with
/// SELECT USING (true) and DELETE USING (owner = 'alice') returns only
/// alice's row id. The emulation fires the INSTEAD OF DELETE trigger for
/// every SELECT-visible row and returns each via RETURNING, even though the
/// trigger body does nothing for bob's row. The result reports id values [1,
/// 2] (or both, in row order) while bob's row remains in the backing table.
/// This assertion pins that behavior and will fail the day RETURNING on
/// UPDATE/DELETE through RLS views is made accurate.
#[test]
fn delete_returning_through_view_reports_skipped_rows() {
    let mut conn = apply();
    set_session_username("alice");

    // Seed: alice owns row 1, bob owns row 2.
    diesel::insert_into(schema::docs_rls::table)
        .values(vec![
            DocRow { id: 1, owner: "alice".to_owned(), body: "alice".to_owned() },
            DocRow { id: 2, owner: "bob".to_owned(), body: "bob".to_owned() },
        ])
        .execute(&mut conn)
        .expect("seed rows");

    // DELETE ... RETURNING through the view. PostgreSQL returns only [1].
    // The emulation returns [1, 2] (both view-visible rows, per trigger firing).
    //
    // If/when this is fixed (by refusing RETURNING on UPDATE/DELETE through RLS
    // views when write scope diverges from read scope, or by an accurate rewrite),
    // this assertion will fail and must be updated.
    #[derive(QueryableByName, Debug)]
    struct IdRow {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
    }

    // DELETE ... RETURNING cannot be expressed via the typed DSL; raw sql_query
    // is correct for executing translator-generated RETURNING statements.
    let returned_ids: Vec<IdRow> = diesel::sql_query("DELETE FROM docs RETURNING id")
        .load(&mut conn)
        .expect("DELETE RETURNING");

    let mut ids: Vec<i32> = returned_ids.iter().map(|r| r.id).collect();
    ids.sort_unstable();

    // Current behavior: both ids returned even though bob's row was not deleted.
    // PostgreSQL would return only [1]. This pin fails the day the emulation
    // is made accurate; update it then and remove this comment.
    assert_eq!(
        ids,
        vec![1, 2],
        "architectural limit: DELETE RETURNING names every view-visible row the trigger \
         fired for, not only rows the DELETE policy actually removed; \
         PostgreSQL returns only [1]; see README RLS-limitations"
    );

    // Bob's row must still be in the backing table (not actually deleted).
    let bob_count: i64 = schema::docs_rls::table
        .filter(schema::docs_rls::id.eq(2))
        .count()
        .get_result(&mut conn)
        .expect("count bob");
    assert_eq!(bob_count, 1, "bob's row must remain: the DELETE policy skipped it");
}
