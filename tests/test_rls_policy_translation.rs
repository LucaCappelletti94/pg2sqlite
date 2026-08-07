//! R80 phase 2: policy predicates run through the forward expression
//! translator.
//!
//! The RLS pipeline used to format the transformed policy expression straight
//! into the emitted view and trigger SQL, so any PostgreSQL-only operator or
//! function in a policy survived untranslated: `ILIKE` failed at apply time,
//! `now()` and `date_trunc()` created fine and failed on the first read
//! (SQLite resolves view bodies lazily), and `position(... in ...)` emitted a
//! shape SQLite misreads. Every test here seeds the backing table, reads
//! through the policy view, and asserts which rows the policy admits.

mod helpers;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, TranslationOptions};

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit".to_string())
}

/// Applies every translated statement to a fresh in-memory SQLite.
fn apply(pg: &str) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    let stmts = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&options())
        .expect("translate");
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("SQLite rejected output: {e}\n{s}"));
    }
    conn
}

/// Reads the ids the policy view admits, in order.
fn visible_ids(conn: &rusqlite::Connection, view: &str) -> Vec<i64> {
    conn.prepare(&format!("SELECT id FROM {view} ORDER BY id"))
        .expect("the policy view must be readable")
        .query_map([], |row| row.get(0))
        .expect("query view")
        .collect::<Result<_, _>>()
        .expect("read rows")
}

#[test]
fn an_ilike_policy_translates_and_filters() {
    let conn = apply(
        "CREATE TABLE docs (id INT PRIMARY KEY, s TEXT);
         ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON docs FOR SELECT USING (s ILIKE 'a%');",
    );
    conn.execute_batch(
        "INSERT INTO docs_rls (id, s) VALUES (1, 'Apple'), (2, 'banana'), (3, 'avocado');",
    )
    .expect("seed backing table");
    assert_eq!(
        visible_ids(&conn, "docs"),
        vec![1, 3],
        "ILIKE is case insensitive, so Apple and avocado pass and banana does not"
    );
}

#[test]
fn a_now_policy_translates_and_filters() {
    let conn = apply(
        "CREATE TABLE runs (id INT PRIMARY KEY, ts TIMESTAMP);
         ALTER TABLE runs ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON runs FOR SELECT USING (ts < now());",
    );
    conn.execute_batch(
        "INSERT INTO runs_rls (id, ts) VALUES (1, '2000-01-01 00:00:00'), (2, '9999-01-01 00:00:00');",
    )
    .expect("seed backing table");
    assert_eq!(visible_ids(&conn, "runs"), vec![1], "only the past row is before now()");
}

#[test]
fn a_date_trunc_policy_translates_and_filters() {
    let conn = apply(
        "CREATE TABLE events (id INT PRIMARY KEY, ts TIMESTAMP);
         ALTER TABLE events ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON events FOR SELECT
             USING (date_trunc('day', ts) = '2024-03-15 00:00:00');",
    );
    conn.execute_batch(
        "INSERT INTO events_rls (id, ts) VALUES
             (1, '2024-03-15 10:30:00'), (2, '2024-03-16 00:10:00');",
    )
    .expect("seed backing table");
    assert_eq!(
        visible_ids(&conn, "events"),
        vec![1],
        "date_trunc must translate so the day comparison filters"
    );
}

#[test]
fn a_position_policy_translates_and_filters() {
    let conn = apply(
        "CREATE TABLE tags (id INT PRIMARY KEY, s TEXT);
         ALTER TABLE tags ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON tags FOR SELECT USING (position('x' in s) > 0);",
    );
    conn.execute_batch("INSERT INTO tags_rls (id, s) VALUES (1, 'xy'), (2, 'ab');")
        .expect("seed backing table");
    assert_eq!(visible_ids(&conn, "tags"), vec![1], "position() must become instr()");
}

#[test]
fn a_with_check_policy_translates_into_the_write_guard() {
    let conn = apply(
        "CREATE TABLE names (id INT PRIMARY KEY, s TEXT);
         ALTER TABLE names ENABLE ROW LEVEL SECURITY;
         CREATE POLICY sel ON names FOR SELECT USING (true);
         CREATE POLICY ins ON names FOR INSERT WITH CHECK (s ILIKE 'a%');",
    );
    conn.execute_batch("INSERT INTO names (id, s) VALUES (1, 'Ann');")
        .expect("a row passing the translated CHECK inserts through the view");
    let err = conn
        .execute_batch("INSERT INTO names (id, s) VALUES (2, 'bob');")
        .expect_err("a row failing the CHECK must be aborted by the guard");
    assert!(
        err.to_string().contains("row-level security"),
        "the guard names the policy violation: {err}"
    );
    assert_eq!(visible_ids(&conn, "names"), vec![1]);
}
