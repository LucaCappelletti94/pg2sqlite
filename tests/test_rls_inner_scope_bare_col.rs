//! A bare column in a policy subquery's WHERE clause must bind to the innermost
//! scope that declares it, not be blindly prefixed with the outer table alias.
//!
//! `FOR ALL USING` is used here because the view's WHERE clause is built from
//! SELECT-applicable policies. A `FOR UPDATE USING` policy produces `WHERE
//! false` in the view, so the INSTEAD OF trigger never fires. With `FOR ALL`,
//! the view filters by the predicate and updates can reach the trigger.

mod helpers;

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        docs2 (id) {
            id -> Integer,
            owner -> Text,
        }
    }

    diesel::table! {
        docs2_rls (id) {
            id -> Integer,
            owner -> Text,
        }
    }

    diesel::table! {
        members (id) {
            id -> Integer,
            doc_owner -> Text,
        }
    }
}

use schema::{docs2, docs2_rls, members};

/// Apply DDL, seed the backing table before triggers install, then add
/// triggers.
fn apply_with_seed(sql: &str, seed: impl FnOnce(&mut SqliteConnection)) -> SqliteConnection {
    let opts = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    // Emitted DDL cannot be expressed with the typed DSL.
    let stmts = Pg2Sqlite::default().sql(sql).expect("parse").translate(&opts).expect("translate");
    let (triggers, base): (Vec<_>, Vec<_>) =
        stmts.iter().map(|s| s.to_string()).partition(|s| s.starts_with("CREATE TRIGGER"));
    let mut conn = helpers::establish_connection();
    for stmt in &base {
        diesel::sql_query(stmt.clone())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
    }
    seed(&mut conn);
    for stmt in &triggers {
        diesel::sql_query(stmt.clone())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("trigger DDL failed: {e}\n{stmt}"));
    }
    conn
}

/// The bug rewrites bare `id` to `OLD.id` (= docs2.id = 2). No member has
/// `id = 2`, so EXISTS is always false: the backing UPDATE's WHERE never
/// matches and the row is silently left unchanged. The fix leaves bare `id` in
/// the inner scope (members.id), making `m.id = id` a tautology so the
/// predicate reduces to `m.doc_owner = OLD.owner`.
#[test]
fn bare_inner_scope_column_not_rewritten_to_outer_prefix() {
    const SQL: &str = "
        CREATE TABLE docs2 (id INT PRIMARY KEY, owner TEXT);
        CREATE TABLE members (id INT PRIMARY KEY, doc_owner TEXT);
        ALTER TABLE docs2 ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs2_pol ON docs2 FOR ALL USING (
            EXISTS (SELECT 1 FROM members m WHERE m.doc_owner = owner AND m.id = id)
        );
    ";
    let mut conn = apply_with_seed(SQL, |conn| {
        // Both OLD ('bob') and NEW ('carol') must satisfy the policy.
        diesel::insert_into(members::table)
            .values(&[
                (members::id.eq(7), members::doc_owner.eq("bob")),
                (members::id.eq(8), members::doc_owner.eq("carol")),
            ])
            .execute(conn)
            .expect("seed members");
        diesel::insert_into(docs2_rls::table)
            .values((docs2_rls::id.eq(2), docs2_rls::owner.eq("bob")))
            .execute(conn)
            .expect("seed docs2_rls");
    });

    // USING (OLD='bob'): m.doc_owner='bob' AND m.id=m.id → true.
    // WITH CHECK (NEW='carol'): m.doc_owner='carol' AND m.id=m.id → true.
    diesel::update(docs2::table.filter(docs2::id.eq(2)))
        .set(docs2::owner.eq("carol"))
        .execute(&mut conn)
        .expect("update must succeed: both USING and WITH CHECK satisfied");

    let owner: String = docs2_rls::table
        .filter(docs2_rls::id.eq(2))
        .select(docs2_rls::owner)
        .first(&mut conn)
        .expect("row must exist");

    assert_eq!(owner, "carol", "update must have changed the row");
}

/// `owner` is only in `docs2`, not in `members`, so it remains an outer
/// reference rewritten to `OLD.owner`/`NEW.owner`. A row visible in the view
/// (alice has a member) must be updatable; a row not visible (carol has no
/// member) must return 0 rows changed, not silently corrupt the backing table.
#[test]
fn outer_only_column_is_still_prefixed() {
    const SQL: &str = "
        CREATE TABLE docs2 (id INT PRIMARY KEY, owner TEXT);
        CREATE TABLE members (id INT PRIMARY KEY, doc_owner TEXT);
        ALTER TABLE docs2 ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs2_pol ON docs2 FOR ALL USING (
            EXISTS (SELECT 1 FROM members m WHERE m.doc_owner = owner)
        );
    ";
    let mut conn = apply_with_seed(SQL, |conn| {
        diesel::insert_into(members::table)
            .values((members::id.eq(1), members::doc_owner.eq("alice")))
            .execute(conn)
            .expect("seed members");
        diesel::insert_into(docs2_rls::table)
            .values(&[
                (docs2_rls::id.eq(10), docs2_rls::owner.eq("alice")),
                (docs2_rls::id.eq(11), docs2_rls::owner.eq("carol")),
            ])
            .execute(conn)
            .expect("seed docs2_rls");
    });

    // alice is visible in the view (member exists). Update to same value → no
    // error.
    diesel::update(docs2::table.filter(docs2::id.eq(10)))
        .set(docs2::owner.eq("alice"))
        .execute(&mut conn)
        .expect("alice's row must be updatable");

    // carol is not visible (no member) → trigger never fires → 0 rows changed.
    let rows_changed = diesel::update(docs2::table.filter(docs2::id.eq(11)))
        .set(docs2::owner.eq("carol_updated"))
        .execute(&mut conn)
        .expect("execute must not error");

    assert_eq!(rows_changed, 0, "carol is not visible, update must change 0 rows");

    let carol_owner: String = docs2_rls::table
        .filter(docs2_rls::id.eq(11))
        .select(docs2_rls::owner)
        .first(&mut conn)
        .expect("carol row must exist");
    assert_eq!(carol_owner, "carol", "carol's backing row must be unchanged");
}
