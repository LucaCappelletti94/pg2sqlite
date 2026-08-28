//! PostgreSQL combines row-level security policies as
//! `(PERMISSIVE_1 OR PERMISSIVE_2 OR ...) AND RESTRICTIVE_1 AND RESTRICTIVE_2`.
//!
//! Permissive policies OR together to grant access. Restrictive policies AND on
//! top to remove it. The two are not interchangeable, and a table carrying only
//! restrictive policies denies every row because nothing granted access in the
//! first place.
//!
//! The translator used to OR every policy together regardless of type, which
//! turned a restrictive policy into a permissive grant.

mod helpers;

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        /// RLS view over `docs_rls`.
        docs (id) {
            id -> Integer,
            owner_id -> Integer,
            dept -> Text,
        }
    }

    diesel::table! {
        /// Backing table, seeded directly to bypass the view.
        docs_rls (id) {
            id -> Integer,
            owner_id -> Integer,
            dept -> Text,
        }
    }
}

use schema::{docs, docs_rls};

#[derive(Insertable)]
#[diesel(table_name = docs_rls)]
struct BackingRow {
    id: i32,
    owner_id: i32,
    dept: String,
}

#[derive(Insertable)]
#[diesel(table_name = docs)]
struct ViewRow {
    id: i32,
    owner_id: i32,
    dept: String,
}

const TABLE: &str = "
    CREATE TABLE docs (
        id INTEGER PRIMARY KEY,
        owner_id INTEGER NOT NULL,
        dept TEXT NOT NULL
    );
    ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
    CREATE POLICY docs_ins ON docs FOR INSERT WITH CHECK (true);
";

/// Applies the translated DDL to a fresh in-memory SQLite. The emitted SQL is
/// the artifact under test; all other statements use the typed DSL.
fn apply(policies: &str) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let pg = format!("{TABLE}{policies}");
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let translated = Pg2Sqlite::default().sql(&pg)?.translate(&options)?;
    let mut conn = SqliteConnection::establish(":memory:")?;
    for statement in &translated {
        diesel::sql_query(statement.to_string()).execute(&mut conn)?;
    }
    Ok(conn)
}

/// Three rows spanning both dimensions the policies below discriminate on:
/// `owner_id` and `dept`.
fn seed(conn: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    diesel::insert_into(docs_rls::table)
        .values(vec![
            BackingRow { id: 1, owner_id: 7, dept: "eng".to_owned() },
            BackingRow { id: 2, owner_id: 9, dept: "eng".to_owned() },
            BackingRow { id: 3, owner_id: 7, dept: "sales".to_owned() },
        ])
        .execute(conn)?;
    Ok(())
}

fn visible_ids(conn: &mut SqliteConnection) -> Result<Vec<i32>, Box<dyn std::error::Error>> {
    Ok(docs::table.select(docs::id).order(docs::id.asc()).load(conn)?)
}

/// Two permissive policies OR together, so a row satisfying either is visible.
#[test]
fn two_permissive_policies_or_together() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(
        "
        CREATE POLICY by_owner ON docs FOR SELECT USING (owner_id = 7);
        CREATE POLICY by_dept  ON docs FOR SELECT USING (dept = 'eng');
    ",
    )?;
    seed(&mut conn)?;

    // owner_id = 7 gives {1,3}, dept = 'eng' gives {1,2}, union is all three.
    assert_eq!(visible_ids(&mut conn)?, vec![1, 2, 3]);

    Ok(())
}

/// A restrictive policy ANDs on top of the permissive grant, narrowing it.
#[test]
fn restrictive_policy_ands_on_top_of_permissive() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(
        "
        CREATE POLICY by_owner ON docs FOR SELECT USING (owner_id = 7);
        CREATE POLICY dept_only ON docs AS RESTRICTIVE FOR SELECT USING (dept = 'eng');
    ",
    )?;
    seed(&mut conn)?;

    // (owner_id = 7) AND (dept = 'eng') leaves only row 1. ORing them instead
    // would wrongly expose rows 2 and 3 as well.
    assert_eq!(visible_ids(&mut conn)?, vec![1]);

    Ok(())
}

/// Restrictive policies alone grant nothing, so every row is denied.
#[test]
fn restrictive_policy_alone_denies_every_row() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(
        "
        CREATE POLICY dept_only ON docs AS RESTRICTIVE FOR SELECT USING (dept = 'eng');
    ",
    )?;
    seed(&mut conn)?;

    assert_eq!(
        visible_ids(&mut conn)?,
        Vec::<i32>::new(),
        "no permissive policy grants access, so nothing is readable"
    );

    Ok(())
}

/// An explicit `AS PERMISSIVE` behaves exactly like the omitted default.
#[test]
fn explicit_permissive_matches_the_default() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(
        "
        CREATE POLICY by_owner ON docs AS PERMISSIVE FOR SELECT USING (owner_id = 7);
    ",
    )?;
    seed(&mut conn)?;

    assert_eq!(visible_ids(&mut conn)?, vec![1, 3]);

    Ok(())
}

/// Multiple restrictive policies AND together, each narrowing further.
#[test]
fn multiple_restrictive_policies_and_together() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(
        "
        CREATE POLICY any_row ON docs FOR SELECT USING (true);
        CREATE POLICY dept_only ON docs AS RESTRICTIVE FOR SELECT USING (dept = 'eng');
        CREATE POLICY owner_only ON docs AS RESTRICTIVE FOR SELECT USING (owner_id = 7);
    ",
    )?;
    seed(&mut conn)?;

    // true AND (dept = 'eng') AND (owner_id = 7) leaves only row 1.
    assert_eq!(visible_ids(&mut conn)?, vec![1]);

    Ok(())
}

/// A restrictive WITH CHECK must block an INSERT the permissive policy alone
/// would accept. This is the case that catches applying the partition to the
/// view while forgetting the write triggers.
#[test]
fn restrictive_check_blocks_an_insert_the_permissive_allows()
-> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(
        "
        CREATE POLICY any_row ON docs FOR SELECT USING (true);
        CREATE POLICY ins_owner ON docs FOR INSERT WITH CHECK (owner_id = 7);
        CREATE POLICY ins_dept ON docs AS RESTRICTIVE FOR INSERT WITH CHECK (dept = 'eng');
    ",
    )?;

    // Satisfies the permissive check but violates the restrictive one.
    let rejected = diesel::insert_into(docs::table)
        .values(ViewRow { id: 10, owner_id: 7, dept: "sales".to_owned() })
        .execute(&mut conn);
    assert!(rejected.is_err(), "a restrictive WITH CHECK must reject the row");

    // Satisfies both.
    diesel::insert_into(docs::table)
        .values(ViewRow { id: 11, owner_id: 7, dept: "eng".to_owned() })
        .execute(&mut conn)?;

    assert_eq!(docs_rls::table.select(docs_rls::id).load::<i32>(&mut conn)?, vec![11]);

    Ok(())
}

/// A restrictive DELETE policy narrows which rows may be deleted.
#[test]
fn restrictive_policy_narrows_delete() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(
        "
        CREATE POLICY any_row ON docs FOR SELECT USING (true);
        CREATE POLICY del_owner ON docs FOR DELETE USING (owner_id = 7);
        CREATE POLICY del_dept ON docs AS RESTRICTIVE FOR DELETE USING (dept = 'eng');
    ",
    )?;
    seed(&mut conn)?;

    // Row 3 is owner_id = 7 but dept = 'sales', so the restrictive policy
    // protects it. ORing instead would delete it.
    diesel::delete(docs::table.filter(docs::id.eq(3))).execute(&mut conn)?;
    assert_eq!(
        docs_rls::table.select(docs_rls::id).order(docs_rls::id.asc()).load::<i32>(&mut conn)?,
        vec![1, 2, 3],
        "the restrictive DELETE policy must protect row 3"
    );

    // Row 1 satisfies both, so it goes.
    diesel::delete(docs::table.filter(docs::id.eq(1))).execute(&mut conn)?;
    assert_eq!(
        docs_rls::table.select(docs_rls::id).order(docs_rls::id.asc()).load::<i32>(&mut conn)?,
        vec![2, 3]
    );

    Ok(())
}
