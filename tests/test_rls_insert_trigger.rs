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

// ── Two-column BEFORE INSERT maintenance trigger → OR exemption (rls.rs
// 2266-2284) ──

diesel::table! {
    items (id) {
        id -> Integer,
        tag -> Nullable<Text>,
        slug -> Nullable<Text>,
    }
}

diesel::table! {
    items_rls (id) {
        id -> Integer,
        tag -> Nullable<Text>,
        slug -> Nullable<Text>,
    }
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = items_rls)]
struct ItemRow {
    tag: Option<String>,
    slug: Option<String>,
}

/// SELECT and INSERT policies; BEFORE INSERT trigger maintains two columns (tag
/// and slug).
const ITEMS_SCHEMA: &str = "
    CREATE TABLE items (id INT PRIMARY KEY, tag TEXT, slug TEXT);
    ALTER TABLE items ENABLE ROW LEVEL SECURITY;
    CREATE POLICY items_sel ON items FOR SELECT USING (true);
    CREATE POLICY items_ins ON items FOR INSERT WITH CHECK (true);
    CREATE FUNCTION items_maintain() RETURNS TRIGGER AS $$
    BEGIN
        NEW.tag := NEW.tag || '!';
        NEW.slug := upper(NEW.slug);
        RETURN NEW;
    END;
    $$ LANGUAGE plpgsql;
    CREATE TRIGGER items_maint_trg BEFORE INSERT ON items
    FOR EACH ROW EXECUTE FUNCTION items_maintain();
";

fn apply_items(pg: &str) -> SqliteConnection {
    let opts = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let stmts =
        Pg2Sqlite::default().sql(pg).expect("parse").translate_to_sql(&opts).expect("translate");
    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut conn).expect("pragma");
    for stmt in &stmts {
        diesel::sql_query(stmt.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("DDL: {e}\n{stmt}"));
    }
    conn
}

/// Two maintained columns produce an OR-joined exemption WHEN clause with two
/// IS DISTINCT FROM conditions (rls.rs lines 2266-2284).  The INSERT must
/// succeed because the maintenance update passes the WHEN clause.
#[test]
fn rls_two_column_maintenance_trigger_exemption_fires() {
    // Verify the exemption clause has two IS DISTINCT FROM conditions.
    let opts = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let stmts = Pg2Sqlite::default()
        .sql(ITEMS_SCHEMA)
        .expect("parse")
        .translate_to_sql(&opts)
        .expect("translate");
    let update_check = stmts
        .iter()
        .map(ToString::to_string)
        .find(|s| s.contains("update_check"))
        .expect("update-check trigger must be emitted");
    assert_eq!(
        update_check.matches("IS DISTINCT FROM").count(),
        2,
        "two maintained columns must produce two IS DISTINCT FROM clauses: {update_check}"
    );
    assert!(update_check.contains(" OR "), "conditions must be joined by OR: {update_check}");

    // Execute: INSERT through the view must succeed.
    let mut conn = apply_items(ITEMS_SCHEMA);
    diesel::insert_into(items::table)
        .values((items::id.eq(1_i32), items::tag.eq("hello"), items::slug.eq("world")))
        .execute(&mut conn)
        .expect("INSERT through RLS view must succeed");
    let row = items_rls::table
        .filter(items_rls::id.eq(1))
        .select(ItemRow::as_select())
        .first(&mut conn)
        .expect("row must be readable from backing table");
    assert_eq!(row.tag.as_deref(), Some("hello!"), "tag must be maintained");
    assert_eq!(row.slug.as_deref(), Some("WORLD"), "slug must be maintained");
}

// ── Non-maintenance BEFORE INSERT trigger → continue at rls.rs 2263 ──────────

diesel::table! {
    notes (id) {
        id -> Integer,
        body -> Nullable<Text>,
    }
}

diesel::table! {
    notes_rls (id) {
        id -> Integer,
        body -> Nullable<Text>,
    }
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = notes_rls)]
struct NoteRow {
    body: Option<String>,
}

/// RLS table with a BEFORE INSERT trigger that does NOT assign to NEW — not a
/// maintenance trigger.  `build_maintenance_insert_exemption` hits the
/// `continue` at rls.rs:2263 and returns None (no exemption in the
/// update-check trigger).  INSERT through the view must still succeed.
const NOTES_SCHEMA: &str = "
    CREATE TABLE notes (id INT PRIMARY KEY, body TEXT);
    ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
    CREATE POLICY notes_sel ON notes FOR SELECT USING (true);
    CREATE POLICY notes_ins ON notes FOR INSERT WITH CHECK (true);
    CREATE FUNCTION notes_passthru() RETURNS TRIGGER AS $$
    BEGIN
        RETURN NEW;
    END;
    $$ LANGUAGE plpgsql;
    CREATE TRIGGER notes_trg BEFORE INSERT ON notes
    FOR EACH ROW EXECUTE FUNCTION notes_passthru();
";

/// The non-maintenance trigger is skipped in the exemption builder; no
/// IS DISTINCT FROM appears in the update-check trigger, and INSERT succeeds.
#[test]
fn rls_non_maintenance_before_insert_trigger_has_no_exemption() {
    let opts = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let stmts = Pg2Sqlite::default()
        .sql(NOTES_SCHEMA)
        .expect("parse")
        .translate_to_sql(&opts)
        .expect("translate");
    let update_check = stmts
        .iter()
        .map(ToString::to_string)
        .find(|s| s.contains("update_check"))
        .expect("update-check trigger must be emitted");
    assert!(
        !update_check.contains("IS DISTINCT FROM"),
        "non-maintenance trigger must not produce an IS DISTINCT FROM exemption: {update_check}"
    );

    let mut conn = {
        let mut c = SqliteConnection::establish(":memory:").expect("connect");
        diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut c).expect("pragma");
        for stmt in &stmts {
            diesel::sql_query(stmt.as_str())
                .execute(&mut c)
                .unwrap_or_else(|e| panic!("DDL: {e}\n{stmt}"));
        }
        c
    };
    diesel::insert_into(notes::table)
        .values((notes::id.eq(1_i32), notes::body.eq("hello")))
        .execute(&mut conn)
        .expect("INSERT must succeed");
    let row = notes_rls::table
        .filter(notes_rls::id.eq(1))
        .select(NoteRow::as_select())
        .first(&mut conn)
        .expect("row must be readable");
    assert_eq!(row.body.as_deref(), Some("hello"), "body must be stored as-is");
}
