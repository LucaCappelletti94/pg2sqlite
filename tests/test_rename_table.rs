//! A table rename is accepted in the one spelling PostgreSQL has, and refused
//! in the three it does not.
//!
//! PostgreSQL accepts `ALTER TABLE <name> RENAME TO <bare name>` and nothing
//! else. `sqlparser`'s PostgreSQL dialect is looser and also parses `RENAME
//! TABLE a TO b`, `ALTER TABLE t RENAME AS t2`, and a schema-qualified rename
//! target. All three were verified rejected by PostgreSQL 16, so a file
//! containing one is not the input this crate translates, and all three are
//! syntax errors in SQLite too: `near "RENAME"`, `near "AS"`, and `near "."`
//! respectively, verified on 3.51.1.
//!
//! Before this, the two AST shapes disagreed.
//! `AlterTableOperation::RenameTable` was translated while
//! `Statement::RenameTable` failed the whole translation with a schema lookup
//! error naming a table the author never mentioned. Both now produce a decision
//! of their own, and only one spelling is accepted, so the two cannot drift
//! apart again.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        /// `t` after the rename, columns carried over.
        t2 (id) {
            id -> Integer,
            a -> Text,
        }
    }
}

/// Emitted SQLite for `pg`, one string per statement.
fn translate(pg: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
}

/// Translates `pg` and applies every emitted statement. The emitted DDL is the
/// artifact under test, so it is applied as generated text.
fn apply(pg: &str) -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    for statement in translate(pg) {
        diesel::sql_query(statement).execute(&mut conn).expect("apply emitted statement");
    }
    conn
}

/// The translation error message for `pg`. Panics when it succeeds.
fn translate_err(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("expected a translation error")
        .to_string()
}

const BASE: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT NOT NULL);";

/// The PostgreSQL spelling renames the table, and the renamed table keeps its
/// columns.
#[test]
fn alter_table_rename_to_renames_the_table() {
    let mut conn = apply(&format!("{BASE} ALTER TABLE t RENAME TO t2;"));

    diesel::insert_into(schema::t2::table)
        .values((schema::t2::id.eq(1), schema::t2::a.eq("kept")))
        .execute(&mut conn)
        .expect("insert into the renamed table");

    let row: (i32, String) = schema::t2::table
        .select((schema::t2::id, schema::t2::a))
        .first(&mut conn)
        .expect("read the renamed table");
    assert_eq!(row, (1, "kept".to_owned()));
}

/// A qualified name for the table being renamed is ordinary PostgreSQL and must
/// keep working. Guards the qualified-target rejection below from swallowing
/// it: the restriction is on the new name, not the old one.
#[test]
fn a_qualified_source_table_still_renames() {
    let mut conn = apply(&format!("{BASE} ALTER TABLE public.t RENAME TO t2;"));

    assert_eq!(schema::t2::table.count().get_result::<i64>(&mut conn).expect("count"), 0);
}

/// `RENAME TABLE` is MySQL. PostgreSQL rejects it, so the input is not
/// PostgreSQL, and the error says so and shows the PostgreSQL spelling.
#[test]
fn rename_table_statement_is_rejected() {
    let error = translate_err(&format!("{BASE} RENAME TABLE t TO t2;"));
    assert!(error.contains("MySQL"), "expected the error to name the dialect, got {error}");
    assert!(
        error.contains("ALTER TABLE t RENAME TO t2"),
        "expected the error to show the PostgreSQL spelling, got {error}"
    );
}

/// The suggestion covers every pair, since `RENAME TABLE` takes a list and an
/// author who wrote three renames needs all three rewritten.
#[test]
fn rename_table_rejection_rewrites_every_pair() {
    let error = translate_err(
        "CREATE TABLE a (id INTEGER PRIMARY KEY);
         CREATE TABLE b (id INTEGER PRIMARY KEY);
         RENAME TABLE a TO a2, b TO b2;",
    );
    assert!(
        error.contains("ALTER TABLE a RENAME TO a2")
            && error.contains("ALTER TABLE b RENAME TO b2"),
        "expected both pairs in the suggestion, got {error}"
    );
}

/// `RENAME AS` is MySQL as well, and is refused rather than quietly rewritten
/// to `RENAME TO`.
#[test]
fn rename_as_is_rejected() {
    let error = translate_err(&format!("{BASE} ALTER TABLE t RENAME AS t2;"));
    assert!(error.contains("RENAME AS"), "expected the error to name the clause, got {error}");
}

/// A rename never moves a table between schemas, so PostgreSQL rejects a
/// qualified target and so does this.
#[test]
fn a_qualified_rename_target_is_rejected() {
    let error = translate_err(&format!("{BASE} ALTER TABLE t RENAME TO public.t2;"));
    assert!(
        error.contains("public.t2"),
        "expected the error to name the offending target, got {error}"
    );
}
