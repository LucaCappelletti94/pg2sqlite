//! F4: `ON CONFLICT ... DO NOTHING` is an upsert clause, not `INSERT OR
//! IGNORE`.
//!
//! PostgreSQL's `DO NOTHING` suppresses exactly one thing, a conflict on the
//! arbiter index. SQLite's `OR IGNORE` suppresses every constraint failure the
//! statement can raise, so a CHECK or NOT NULL violation that PostgreSQL
//! reports became a silently skipped row. SQLite has had the upsert clause
//! since 3.24 and it draws the same line PostgreSQL draws, so the clause is
//! kept rather than traded for a conflict-resolution keyword.
//!
//! Every expectation was read off PostgreSQL 17 before the fix. The emitted
//! statements are executed as text because that text is the artifact under
//! test; everything read back afterwards goes through the typed diesel DSL.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        oc (id) {
            id -> Integer,
            n -> Integer,
            m -> Integer,
        }
    }

    diesel::table! {
        uq (id) {
            id -> Integer,
            tag -> Text,
        }
    }
}

/// A table that can fail three different ways: a primary key conflict, a
/// CHECK, and a NOT NULL. Only the first is PostgreSQL's to suppress.
const FIXTURE: &str =
    "CREATE TABLE oc (id INT PRIMARY KEY, n INT CHECK (n > 0), m INT NOT NULL DEFAULT 1);
     CREATE TABLE uq (id INT PRIMARY KEY, tag TEXT, CONSTRAINT uq_tag UNIQUE (tag));
     CREATE TABLE src (id INT, n INT);
     INSERT INTO oc (id, n) VALUES (1, 5);
     INSERT INTO uq (id, tag) VALUES (1, 'a');
     INSERT INTO src (id, n) VALUES (1, 7), (2, 8);";

fn translate(dml: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(&format!("{FIXTURE}\n{dml}"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
}

/// Runs every emitted statement, and panics if any of them fails.
fn apply(dml: &str) -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    for statement in translate(dml) {
        diesel::sql_query(&statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted statement failed: {statement}: {error}"));
    }
    connection
}

/// The first execution failure, for the cases where PostgreSQL raises one.
fn apply_err(dml: &str) -> diesel::result::Error {
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    for statement in translate(dml) {
        if let Err(error) = diesel::sql_query(&statement).execute(&mut connection) {
            return error;
        }
    }
    panic!("every emitted statement succeeded, expected one to fail");
}

fn rows(connection: &mut SqliteConnection) -> Vec<(i32, i32, i32)> {
    schema::oc::table
        .select((schema::oc::id, schema::oc::n, schema::oc::m))
        .order(schema::oc::id)
        .load(connection)
        .expect("load")
}

// ---------------------------------------------------------------------------
// What DO NOTHING must keep suppressing
// ---------------------------------------------------------------------------

/// The conflict itself is still ignored, which is the behaviour `OR IGNORE`
/// got right and this must not lose.
#[test]
fn a_conflict_on_the_target_is_ignored() {
    let mut connection = apply("INSERT INTO oc (id, n) VALUES (1, 9) ON CONFLICT (id) DO NOTHING;");
    assert_eq!(rows(&mut connection), vec![(1, 5, 1)]);
}

/// PostgreSQL takes a missing conflict target to mean any unique constraint,
/// and so does SQLite.
#[test]
fn a_targetless_conflict_is_ignored() {
    let mut connection = apply("INSERT INTO oc (id, n) VALUES (1, 9) ON CONFLICT DO NOTHING;");
    assert_eq!(rows(&mut connection), vec![(1, 5, 1)]);
}

/// A constraint named in the statement resolves to the columns behind it, the
/// same lookup `DO UPDATE` already used.
#[test]
fn a_named_constraint_target_is_ignored() {
    let mut connection = apply(
        "INSERT INTO uq (id, tag) VALUES (2, 'a') ON CONFLICT ON CONSTRAINT uq_tag DO NOTHING;",
    );
    let stored: Vec<(i32, String)> = schema::uq::table
        .select((schema::uq::id, schema::uq::tag))
        .load(&mut connection)
        .expect("load");
    assert_eq!(stored, vec![(1, "a".to_owned())]);
}

// ---------------------------------------------------------------------------
// What DO NOTHING must stop suppressing
// ---------------------------------------------------------------------------

/// The item's own trigger. PostgreSQL reports `new row for relation "oc"
/// violates check constraint "oc_n_check"`, while `OR IGNORE` swallowed the
/// row and reported success.
#[test]
fn a_check_violation_is_not_swallowed() {
    let error = apply_err("INSERT INTO oc (id, n) VALUES (2, -5) ON CONFLICT (id) DO NOTHING;");
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "the CHECK must reach the caller: {error}"
    );
}

/// A NOT NULL violation is not a conflict either.
#[test]
fn a_not_null_violation_is_not_swallowed() {
    let error =
        apply_err("INSERT INTO oc (id, n, m) VALUES (3, 5, NULL) ON CONFLICT (id) DO NOTHING;");
    assert!(
        error.to_string().contains("NOT NULL constraint failed"),
        "the NOT NULL must reach the caller: {error}"
    );
}

/// Dropping the conflict target does not widen what is suppressed.
#[test]
fn a_targetless_do_nothing_still_raises_a_check_violation() {
    let error = apply_err("INSERT INTO oc (id, n) VALUES (4, -5) ON CONFLICT DO NOTHING;");
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "the CHECK must reach the caller: {error}"
    );
}

/// The conflict resolution keyword is gone from the output, which is what
/// makes the three refusals above possible.
#[test]
fn the_output_no_longer_carries_or_ignore() {
    let emitted = translate("INSERT INTO oc (id, n) VALUES (1, 9) ON CONFLICT (id) DO NOTHING;");
    let insert = emitted.last().expect("an emitted insert");
    assert!(!insert.contains("OR IGNORE"), "the resolution keyword must be gone: {insert}");
    assert!(insert.contains("ON CONFLICT"), "the upsert clause must survive: {insert}");
}

// ---------------------------------------------------------------------------
// The SELECT source SQLite will not parse without a WHERE
// ---------------------------------------------------------------------------

/// SQLite cannot tell an upsert clause from the tail of a bare SELECT, and
/// its own documentation says to write a WHERE clause even a trivial one.
/// Without it the emitted statement answers `near "DO": syntax error`.
#[test]
fn a_select_source_gets_the_where_sqlite_needs() {
    let mut connection =
        apply("INSERT INTO oc (id, n) SELECT id, n FROM src ON CONFLICT (id) DO NOTHING;");
    assert_eq!(rows(&mut connection), vec![(1, 5, 1), (2, 8, 1)]);
}

/// The same statement was already unrunnable on the DO UPDATE path, which
/// never had anything to do with DO NOTHING becoming OR IGNORE.
#[test]
fn a_select_source_upsert_gets_it_too() {
    let mut connection = apply(
        "INSERT INTO oc (id, n) SELECT id, n FROM src ON CONFLICT (id) DO UPDATE SET n = EXCLUDED.n;",
    );
    assert_eq!(rows(&mut connection), vec![(1, 7, 1), (2, 8, 1)]);
}

/// A source that already ends in something SQLite can tell apart is left
/// alone, so the WHERE is added where it is needed and nowhere else.
#[test]
fn a_source_that_already_disambiguates_is_untouched() {
    let emitted = translate(
        "INSERT INTO oc (id, n) SELECT id, n FROM src WHERE id > 1 ON CONFLICT (id) DO NOTHING;",
    );
    let insert = emitted.last().expect("an emitted insert");
    assert!(!insert.contains("true"), "no second WHERE was needed: {insert}");
    let mut connection = apply(
        "INSERT INTO oc (id, n) SELECT id, n FROM src WHERE id > 1 ON CONFLICT (id) DO NOTHING;",
    );
    assert_eq!(rows(&mut connection), vec![(1, 5, 1), (2, 8, 1)]);
}

/// A VALUES source was never ambiguous and must not grow a WHERE.
#[test]
fn a_values_source_is_untouched() {
    let emitted = translate("INSERT INTO oc (id, n) VALUES (9, 9) ON CONFLICT (id) DO NOTHING;");
    let insert = emitted.last().expect("an emitted insert");
    assert!(!insert.contains("WHERE"), "a VALUES source needs no WHERE: {insert}");
}

// ---------------------------------------------------------------------------
// RETURNING
// ---------------------------------------------------------------------------

#[derive(QueryableByName)]
struct Returned {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
}

/// PostgreSQL returns only the rows the statement actually inserted, so the
/// ignored one must not appear.
#[test]
fn returning_reports_only_the_inserted_row() {
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    let emitted = translate(
        "INSERT INTO oc (id, n) VALUES (1, 9), (7, 7) ON CONFLICT (id) DO NOTHING RETURNING id;",
    );
    let (probe, setup) = emitted.split_last().expect("an emitted insert");
    for statement in setup {
        diesel::sql_query(statement).execute(&mut connection).expect("emitted setup");
    }
    let returned: Vec<i32> = diesel::sql_query(probe)
        .load::<Returned>(&mut connection)
        .expect("returning")
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(returned, vec![7]);
}

// ---------------------------------------------------------------------------
// Held for upstream
// ---------------------------------------------------------------------------

/// PostgreSQL lets the conflict target name a partial unique index by
/// carrying its predicate, and `sqlparser` has neither the parse branch nor
/// a field to hold one, so the statement never reaches the translator. Pinned
/// here so it fails the day upstream gains support and this becomes
/// translatable work rather than a parse error. See
/// `upstream/sqlparser-on-conflict-index-predicate.md`.
#[test]
fn the_index_predicate_target_does_not_reach_the_translator_yet() {
    let error = Pg2Sqlite::default()
        .sql(&format!(
            "{FIXTURE}\nINSERT INTO oc (id, n) VALUES (1, 9) ON CONFLICT (id) WHERE n > 0 DO NOTHING;"
        ))
        .expect_err("sqlparser cannot parse an index predicate on a conflict target")
        .to_string();
    assert!(
        error.contains("Expected: DO, found: WHERE"),
        "the parse must still fail on the predicate: {error}"
    );
}
