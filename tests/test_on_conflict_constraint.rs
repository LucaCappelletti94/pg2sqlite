//! `ON CONFLICT ON CONSTRAINT <name> DO UPDATE` must name columns in the
//! output, because SQLite's conflict target accepts a column list or nothing.
//!
//! The clause used to be cloned through, emitting `ON CONFLICT ON CONSTRAINT
//! t_v_key DO UPDATE ...`, which SQLite rejects with `near "ON": syntax error`.
//! Dropping the target instead is not an option: `ON CONFLICT DO UPDATE` with
//! no target is a syntax error in SQLite too, and even where it parses it would
//! change which conflicts are caught.
//!
//! Resolution covers the three ways a PostgreSQL constraint gets its name,
//! verified against PostgreSQL 16 by reading `pg_constraint` for a table
//! declaring all of them: an explicit `CONSTRAINT <name> UNIQUE (...)` keeps
//! that name, a unique constraint is auto-named `<table>_<column>_key` with
//! every column joined by an underscore, and a primary key is auto-named
//! `<table>_pkey`.
//!
//! Note the asymmetry this closes: `DO NOTHING` with the same target already
//! worked, since it becomes `INSERT OR IGNORE` and the target is irrelevant.
//! Only `DO UPDATE` was broken.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        t (id) {
            id -> Integer,
            v -> Text,
            n -> Integer,
        }
    }

    diesel::table! {
        pair (id) {
            id -> Integer,
            a -> Integer,
            b -> Integer,
            n -> Integer,
        }
    }
}

fn translate(pg: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
}

fn apply(pg: &str) -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    for statement in translate(pg) {
        diesel::sql_query(&statement)
            .execute(&mut conn)
            .unwrap_or_else(|error| panic!("emitted statement failed: {statement}: {error}"));
    }
    conn
}

fn translate_err(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("expected a translation error")
        .to_string()
}

/// A table whose unique constraint is named explicitly.
const NAMED: &str = "CREATE TABLE t (
        id INTEGER PRIMARY KEY,
        v TEXT NOT NULL,
        n INTEGER NOT NULL,
        CONSTRAINT v_is_unique UNIQUE (v)
    );
    INSERT INTO t (id, v, n) VALUES (1, 'a', 1);";

/// The same table relying on PostgreSQL's automatic constraint naming.
const AUTO: &str = "CREATE TABLE t (
        id INTEGER PRIMARY KEY,
        v TEXT NOT NULL UNIQUE,
        n INTEGER NOT NULL
    );
    INSERT INTO t (id, v, n) VALUES (1, 'a', 1);";

/// Asserts the upsert landed on the existing row rather than inserting a
/// second one, which is what proves the conflict target survived.
fn assert_upserted(conn: &mut SqliteConnection) {
    let rows: Vec<(i32, String, i32)> = schema::t::table
        .select((schema::t::id, schema::t::v, schema::t::n))
        .load(conn)
        .expect("load");
    assert_eq!(rows, vec![(1, "a".to_owned(), 5)]);
}

/// An explicitly named constraint resolves to its column list.
#[test]
fn an_explicitly_named_constraint_resolves() {
    let mut conn = apply(&format!(
        "{NAMED}
         INSERT INTO t (id, v, n) VALUES (2, 'a', 5)
         ON CONFLICT ON CONSTRAINT v_is_unique DO UPDATE SET n = EXCLUDED.n;"
    ));

    assert_upserted(&mut conn);
}

/// PostgreSQL names an anonymous unique constraint `<table>_<column>_key`, and
/// that is the name a migration written against PostgreSQL will use.
#[test]
fn an_auto_named_unique_constraint_resolves() {
    let mut conn = apply(&format!(
        "{AUTO}
         INSERT INTO t (id, v, n) VALUES (2, 'a', 5)
         ON CONFLICT ON CONSTRAINT t_v_key DO UPDATE SET n = EXCLUDED.n;"
    ));

    assert_upserted(&mut conn);
}

/// A primary key is auto-named `<table>_pkey`.
#[test]
fn the_primary_key_constraint_resolves() {
    let mut conn = apply(&format!(
        "{AUTO}
         INSERT INTO t (id, v, n) VALUES (1, 'a', 5)
         ON CONFLICT ON CONSTRAINT t_pkey DO UPDATE SET n = EXCLUDED.n;"
    ));

    assert_upserted(&mut conn);
}

/// A multi-column constraint joins every column into the name, and every
/// column has to come back out.
#[test]
fn a_multi_column_constraint_resolves() {
    let mut conn = apply(
        "CREATE TABLE pair (
            id INTEGER PRIMARY KEY,
            a INTEGER NOT NULL,
            b INTEGER NOT NULL,
            n INTEGER NOT NULL,
            UNIQUE (a, b)
         );
         INSERT INTO pair (id, a, b, n) VALUES (1, 1, 2, 1);
         INSERT INTO pair (id, a, b, n) VALUES (2, 1, 2, 5)
         ON CONFLICT ON CONSTRAINT pair_a_b_key DO UPDATE SET n = EXCLUDED.n;",
    );

    let rows: Vec<(i32, i32, i32, i32)> = schema::pair::table
        .select((schema::pair::id, schema::pair::a, schema::pair::b, schema::pair::n))
        .load(&mut conn)
        .expect("load");
    assert_eq!(rows, vec![(1, 1, 2, 5)]);
}

/// A constraint the schema does not hold is refused. Dropping the target would
/// change which conflicts are caught, so guessing is not acceptable.
#[test]
fn an_unknown_constraint_is_rejected() {
    let error = translate_err(&format!(
        "{AUTO}
         INSERT INTO t (id, v, n) VALUES (2, 'a', 5)
         ON CONFLICT ON CONSTRAINT no_such_constraint DO UPDATE SET n = EXCLUDED.n;"
    ));

    assert!(
        error.contains("no_such_constraint"),
        "expected the error to name the constraint, got {error}"
    );
}

/// Guards the fix: `DO NOTHING` already worked, becoming `INSERT OR IGNORE`,
/// and must keep working with a named constraint it never needed to resolve.
#[test]
fn do_nothing_with_a_named_constraint_still_ignores() {
    let mut conn = apply(&format!(
        "{AUTO}
         INSERT INTO t (id, v, n) VALUES (2, 'a', 5)
         ON CONFLICT ON CONSTRAINT t_v_key DO NOTHING;"
    ));

    let rows: Vec<(i32, String, i32)> = schema::t::table
        .select((schema::t::id, schema::t::v, schema::t::n))
        .load(&mut conn)
        .expect("load");
    assert_eq!(rows, vec![(1, "a".to_owned(), 1)], "the conflicting row must be ignored");
}

/// Guards the fix from touching the ordinary column-list target.
#[test]
fn a_column_list_target_still_upserts() {
    let mut conn = apply(&format!(
        "{AUTO}
         INSERT INTO t (id, v, n) VALUES (2, 'a', 5)
         ON CONFLICT (v) DO UPDATE SET n = EXCLUDED.n;"
    ));

    assert_upserted(&mut conn);
}
