#![allow(missing_docs)]
//! Finding 2: an INSERT through an RLS view must succeed when the table has a
//! BEFORE INSERT maintenance trigger that mutates NEW but no UPDATE policy.
//! PostgreSQL ground truth: INSERT (1,'a') → (1,'a*'), DELETE succeeds.

use diesel::{Connection, QueryResult, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// The RLS view is the user-visible surface; backing table is for result reads.
diesel::table! {
    rts (id) {
        id -> Integer,
        tag -> Nullable<Text>,
    }
}

diesel::table! {
    rts_rls (id) {
        id -> Integer,
        tag -> Nullable<Text>,
    }
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = rts_rls)]
struct RtsRow {
    id: i32,
    tag: Option<String>,
}

/// SELECT, INSERT (id>0), DELETE policies; no UPDATE policy.
const RTS_SCHEMA: &str = "
    CREATE TABLE rts (id INT PRIMARY KEY, tag TEXT);
    ALTER TABLE rts ENABLE ROW LEVEL SECURITY;
    CREATE POLICY rts_sel ON rts FOR SELECT USING (true);
    CREATE POLICY rts_ins ON rts FOR INSERT WITH CHECK (id > 0);
    CREATE POLICY rts_del ON rts FOR DELETE USING (true);
    CREATE FUNCTION rts_tag() RETURNS trigger AS $$
    BEGIN
        NEW.tag := NEW.tag || '*';
        RETURN NEW;
    END;
    $$ LANGUAGE plpgsql;
    CREATE TRIGGER rts_tag_trg BEFORE INSERT ON rts FOR EACH ROW EXECUTE FUNCTION rts_tag();
";

fn apply(pg: &str) -> SqliteConnection {
    let opts = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let stmts =
        Pg2Sqlite::default().sql(pg).expect("parse").translate_to_sql(&opts).expect("translate");
    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    // Translated DDL is not expressible via diesel's typed DSL.
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut conn).expect("pragma");
    for stmt in &stmts {
        diesel::sql_query(stmt.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("DDL: {e}\n{stmt}"));
    }
    conn
}

fn insert_via_view(conn: &mut SqliteConnection, id: i32, tag: &str) -> QueryResult<usize> {
    // INSERT goes through the INSTEAD OF INSERT trigger on the RLS view.
    diesel::sql_query(format!("INSERT INTO rts (id, tag) VALUES ({id}, '{tag}')")).execute(conn)
}

/// PostgreSQL: INSERT (1,'a') succeeds and tag becomes 'a*'.
#[test]
fn insert_through_rls_view_with_before_insert_trigger_applies_mutation() {
    let mut conn = apply(RTS_SCHEMA);
    insert_via_view(&mut conn, 1, "a").expect("INSERT must succeed");

    let rows = rts_rls::table
        .select(RtsRow::as_select())
        .load::<RtsRow>(&mut conn)
        .expect("select from backing table");

    assert_eq!(rows.len(), 1, "one row expected");
    assert_eq!(rows[0].id, 1);
    assert_eq!(rows[0].tag.as_deref(), Some("a*"), "trigger must have appended '*'");
}

/// The INSERT policy (id > 0) must still reject id = 0.
#[test]
fn insert_policy_still_enforced_when_trigger_is_present() {
    let mut conn = apply(RTS_SCHEMA);
    let result = insert_via_view(&mut conn, 0, "x");
    assert!(result.is_err(), "id=0 violates INSERT WITH CHECK (id > 0)");
}

/// DELETE through the view must succeed after the INSERT.
#[test]
fn delete_after_insert_succeeds() {
    let mut conn = apply(RTS_SCHEMA);
    insert_via_view(&mut conn, 1, "a").expect("insert");
    diesel::sql_query("DELETE FROM rts WHERE id = 1")
        .execute(&mut conn)
        .expect("DELETE through view must succeed");

    let count: i64 = rts_rls::table.count().get_result(&mut conn).expect("count");
    assert_eq!(count, 0);
}

/// Direct UPDATE to the backing table must still be denied when there is no
/// UPDATE policy; the maintenance exemption does not cover caller writes.
#[test]
fn direct_backing_update_denied_without_update_policy() {
    let mut conn = apply(RTS_SCHEMA);
    insert_via_view(&mut conn, 1, "a").expect("insert");

    // This is a caller update directly to the backing table, not a maintenance
    // write; a different tag value ensures the exemption WHEN clause does not
    // match.
    let result =
        diesel::sql_query("UPDATE rts_rls SET tag = 'evil' WHERE id = 1").execute(&mut conn);
    assert!(result.is_err(), "direct backing UPDATE must be denied when no UPDATE policy exists");
}
