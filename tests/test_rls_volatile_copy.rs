//! D1 and D2 from the round-two partial-walk-defects audit.
//!
//! A volatile DEFAULT on a column the INSERT policy reads appears in the guard
//! substitution (RAISE check) and again in the forwarding INSERT VALUES, two
//! independent draws. A volatile USING predicate appears in the INSTEAD OF
//! trigger RAISE guard and again in the forwarding UPDATE WHERE clause.
//!
//! Measurement: a volatile DEFAULT on a column NO policy reads appears only in
//! the forwarding INSERT VALUES (one position, no duplication, no refusal).

mod helpers;

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit")
}

fn translate_to_sql(schema: &str) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    Pg2Sqlite::default().sql(schema).expect("parse").translate_to_sql(&opts())
}

fn refusal(schema: &str) -> String {
    translate_to_sql(schema)
        .expect_err("translation must be refused for a volatile operand")
        .to_string()
}

fn apply(schema: &str) -> SqliteConnection {
    let stmts = translate_to_sql(schema).expect("translation must succeed");
    let mut conn = SqliteConnection::establish(":memory:").expect("in-memory SQLite");
    for stmt in stmts {
        diesel::sql_query(stmt).execute(&mut conn).expect("apply DDL");
    }
    conn
}

// Diesel schemas for typed reads and writes in the success tests.

mod score_schema {
    diesel::table! {
        /// The RLS view callers write through.
        docs (id) {
            id -> Integer,
            score -> Integer,
        }
    }
    diesel::table! {
        /// The backing table, read directly so assertions see what was stored.
        docs_rls (id) {
            id -> Integer,
            score -> Integer,
        }
    }
}

mod owner_schema {
    diesel::table! {
        /// The RLS view callers write through.
        docs (id) {
            id -> Integer,
            owner -> Text,
        }
    }
    diesel::table! {
        /// The backing table, read directly so assertions see what was stored.
        docs_rls (id) {
            id -> Integer,
            owner -> Text,
        }
    }
}

// ── D1: volatile DEFAULT in the INSERT path ────────────────────────────────

/// The INSERT guard folds `COALESCE(NEW.luck, random())` into the policy check,
/// and the forwarding INSERT VALUES also carries `COALESCE(NEW.luck,
/// random())`.
#[test]
fn volatile_default_read_by_policy_is_refused() {
    let err = refusal(
        "
        CREATE TABLE docs (
            id INTEGER PRIMARY KEY,
            luck FLOAT NOT NULL DEFAULT random()
        );
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_p ON docs FOR ALL USING (luck > 0.5);
    ",
    );
    assert!(err.contains("luck"), "refusal must name the column: {err}");
}

/// A deterministic DEFAULT on a column the policy reads translates and the row
/// lands with its default value when the caller omits the column.
#[test]
fn deterministic_default_read_by_policy_translates_and_lands_row() {
    use score_schema::{docs, docs_rls};

    let mut conn = apply(
        "
        CREATE TABLE docs (
            id INTEGER PRIMARY KEY,
            score INTEGER NOT NULL DEFAULT 42
        );
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_p ON docs FOR ALL USING (score > 0);
    ",
    );
    diesel::insert_into(docs::table)
        .values(docs::id.eq(1))
        .execute(&mut conn)
        .expect("insert through view");
    let score: i32 = docs_rls::table
        .select(docs_rls::score)
        .filter(docs_rls::id.eq(1))
        .first(&mut conn)
        .expect("row in backing table");
    assert_eq!(score, 42);
}

// ── D2: volatile USING predicate ───────────────────────────────────────────

/// The USING predicate `random() > 0` is non-replayable: it lands in the RAISE
/// guard and again in the forwarding UPDATE WHERE, two independent draws.
#[test]
fn volatile_update_using_predicate_is_refused() {
    let err = refusal(
        "
        CREATE TABLE docs (
            id INTEGER PRIMARY KEY,
            owner TEXT NOT NULL
        );
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_p ON docs FOR UPDATE USING (random() > 0);
    ",
    );
    assert!(err.contains("docs_p"), "refusal must name the policy: {err}");
}

/// A deterministic USING predicate translates and a write through the view
/// lands the row.
#[test]
fn deterministic_policy_predicate_translates_and_lands_row() {
    use owner_schema::{docs, docs_rls};

    let mut conn = apply(
        "
        CREATE TABLE docs (
            id INTEGER PRIMARY KEY,
            owner TEXT NOT NULL
        );
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_p ON docs FOR ALL USING (owner = 'alice');
    ",
    );
    diesel::insert_into(docs::table)
        .values((docs::id.eq(1), docs::owner.eq("alice")))
        .execute(&mut conn)
        .expect("insert through view");
    let count: i64 = docs_rls::table
        .count()
        .filter(docs_rls::owner.eq("alice"))
        .get_result(&mut conn)
        .expect("count");
    assert_eq!(count, 1);
}

// ── Measurement: volatile DEFAULT that no policy reads ─────────────────────

/// `luck` has a volatile DEFAULT but the policy reads only `score`, so the
/// guard substitution for `luck` is never folded into the predicate. Only the
/// forwarding INSERT carries it — one output position, no duplication.
#[test]
fn volatile_default_not_read_by_policy_is_not_refused() {
    translate_to_sql(
        "
        CREATE TABLE docs (
            id INTEGER PRIMARY KEY,
            score INTEGER NOT NULL DEFAULT 42,
            luck FLOAT NOT NULL DEFAULT random()
        );
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_p ON docs FOR ALL USING (score > 0);
    ",
    )
    .expect("volatile default not read by the policy must not be refused");
}
