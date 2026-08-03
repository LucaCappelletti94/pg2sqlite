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
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping},
    traits::TranslationOptions,
};

/// Options that resolve the `app.user` setting the RLS tests at the end of this
/// file reference.
fn rls_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_session_user_role("app_user".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting("app.user", "app_user"))
}

/// The emitted script for `sql`, joined.
fn rls_translation(sql: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate(&rls_options())
        .expect("translate")
        .iter()
        .map(ToString::to_string)
        .collect()
}

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

/// Row level security works when no rename is involved. This is the control for
/// the two pins below: identical input bar the rename, wildly different result.
#[test]
fn row_level_security_without_a_rename_builds_its_object_set() {
    let emitted = rls_translation(
        "CREATE TABLE t2 (id INT PRIMARY KEY, owner TEXT);
         ALTER TABLE t2 ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON t2 USING (owner = current_setting('app.user'));",
    )
    .join("\n");

    assert!(
        emitted.contains("CREATE VIEW t2") && emitted.contains("t2_rls"),
        "the control must emit the backing table and the view, got:\n{emitted}"
    );
}

/// Pins a live defect: a rename above `ENABLE ROW LEVEL SECURITY` discards the
/// security setting silently.
///
/// The rename is filtered out of the schema, so `t2` is unknown to it. The
/// `ENABLE ROW LEVEL SECURITY` still reaches the schema builder, whose arm
/// skips an unresolvable table, and the statement itself emits nothing because
/// RLS is realised from the schema rather than from an `ALTER TABLE`. The
/// caller gets an unprotected table and no diagnostic.
///
/// Translating against a schema rebuilt from a growing prefix does not fix
/// this. It was measured: RLS is realised while the `CREATE TABLE` is
/// translated, from an `ENABLE ROW LEVEL SECURITY` written below it, so a
/// prefix that stops at the `CREATE TABLE` cannot know. That attempt failed
/// 165 tests.
#[test]
fn rename_above_row_level_security_still_discards_it() {
    let emitted = rls_translation(
        "CREATE TABLE t (id INT PRIMARY KEY, owner TEXT);
         ALTER TABLE t RENAME TO t2;
         ALTER TABLE t2 ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON t2 USING (owner = current_setting('app.user'));",
    )
    .join("\n");

    assert!(
        !emitted.contains("CREATE VIEW"),
        "the RLS object set is now emitted, so the silent discard is fixed. Unpin this and \
         assert the protection instead:\n{emitted}"
    );
}

/// Pins the other half of the same hole: with the rename below the policy the
/// setting survives, and the emitted script is then invalid.
///
/// RLS turns the table into a backing table plus a view carrying the original
/// name, so the rename lands on the view and SQLite refuses it. Supporting the
/// combination means renaming the backing table, both views, and every trigger
/// together, which is a feature rather than a schema-plumbing fix.
#[test]
fn renaming_a_table_that_has_row_level_security_emits_unrunnable_sql() {
    let emitted = rls_translation(
        "CREATE TABLE t (id INT PRIMARY KEY, owner TEXT);
         ALTER TABLE t ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON t USING (owner = current_setting('app.user'));
         ALTER TABLE t RENAME TO t2;",
    );

    let connection = rusqlite::Connection::open_in_memory().expect("open");
    connection
        .create_scalar_function(
            "app_user",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            |_| Ok("alice".to_string()),
        )
        .expect("register the session user function");

    let failure = emitted
        .iter()
        .find_map(|statement| connection.execute_batch(statement).err().map(|e| e.to_string()));

    let failure = failure.expect(
        "every statement now applies, so renaming an RLS table is supported. Unpin this and \
         assert the renamed object set instead",
    );
    assert!(
        failure.contains("may not be altered"),
        "expected SQLite to refuse the rename of the RLS view, got: {failure}"
    );
}
