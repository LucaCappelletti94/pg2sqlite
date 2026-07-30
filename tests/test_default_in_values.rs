//! `DEFAULT` inside a `VALUES` row must be replaced by the column's declared
//! default, because SQLite accepts the keyword only in `INSERT INTO t DEFAULT
//! VALUES`.
//!
//! Verified on SQLite 3.51.1: `INSERT INTO t (id, s) VALUES (1, DEFAULT)` is
//! `near "DEFAULT": syntax error`. The clause used to be cloned through, so any
//! insert mixing a default with a real value failed to execute.
//!
//! The substitution has to know the column, which means resolving the target
//! table and matching each `DEFAULT` to its position in the column list, the
//! same walk the vector and UUID literal wrappers in `insert.rs` already do.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        t (id) {
            id -> Integer,
            s -> Nullable<Text>,
        }
    }

    diesel::table! {
        stamped (id) {
            id -> Integer,
            created -> Nullable<Text>,
        }
    }
}

fn translate(pg: &str) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    Pg2Sqlite::default().sql(pg)?.translate_to_sql(&Pg2SqliteOptions::default())
}

fn apply(pg: &str) -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    for statement in translate(pg).expect("translate") {
        diesel::sql_query(&statement)
            .execute(&mut conn)
            .unwrap_or_else(|error| panic!("emitted statement failed: {statement}: {error}"));
    }
    conn
}

fn translate_err(pg: &str) -> String {
    translate(pg).expect_err("expected a translation error").to_string()
}

/// The declared default replaces the keyword, and the other row is untouched.
#[test]
fn a_declared_default_is_substituted() {
    let mut conn = apply(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT DEFAULT 'declared');
         INSERT INTO t (id, s) VALUES (1, DEFAULT), (2, 'given');",
    );

    let rows: Vec<(i32, Option<String>)> =
        schema::t::table.select((schema::t::id, schema::t::s)).load(&mut conn).expect("load");
    assert_eq!(rows, vec![(1, Some("declared".to_owned())), (2, Some("given".to_owned()))]);
}

/// A default that is a function call has to be translated too, not copied, or
/// the emitted row would call a PostgreSQL function SQLite does not have.
#[test]
fn a_function_default_is_translated() {
    let mut conn = apply(
        "CREATE TABLE stamped (id INTEGER PRIMARY KEY, created TIMESTAMP DEFAULT now());
         INSERT INTO stamped (id, created) VALUES (1, DEFAULT);",
    );

    let created: Option<String> =
        schema::stamped::table.select(schema::stamped::created).first(&mut conn).expect("load");
    let created = created.expect("the default must have produced a timestamp");
    assert!(
        created.starts_with("20") && created.len() >= 19,
        "expected an ISO timestamp from the translated default, got {created}"
    );
}

/// A generated primary key is the common case: PostgreSQL takes the next
/// sequence value, and SQLite assigns the rowid when the value is NULL, so the
/// two agree and the insert must keep working.
#[test]
fn a_default_generated_primary_key_is_assigned() {
    let mut conn = apply(
        "CREATE TABLE t (id SERIAL PRIMARY KEY, s TEXT);
         INSERT INTO t (id, s) VALUES (DEFAULT, 'first'), (DEFAULT, 'second');",
    );

    let rows: Vec<(i32, Option<String>)> = schema::t::table
        .select((schema::t::id, schema::t::s))
        .order(schema::t::id)
        .load(&mut conn)
        .expect("load");
    assert_eq!(rows, vec![(1, Some("first".to_owned())), (2, Some("second".to_owned()))]);
}

/// A nullable column with no declared default takes NULL, which is what
/// PostgreSQL inserts too.
#[test]
fn a_nullable_column_without_a_default_takes_null() {
    let mut conn = apply(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT);
         INSERT INTO t (id, s) VALUES (1, DEFAULT);",
    );

    let s: Option<String> = schema::t::table.select(schema::t::s).first(&mut conn).expect("load");
    assert_eq!(s, None);
}

/// A NOT NULL column with no declared default has nothing to substitute, and
/// the insert could only ever fail, so it is reported at translation time.
#[test]
fn a_not_null_column_without_a_default_is_rejected() {
    let error = translate_err(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT NOT NULL);
         INSERT INTO t (id, s) VALUES (1, DEFAULT);",
    );

    assert!(error.contains('s'), "expected the error to name the column, got {error}");
}

/// `DEFAULT` is only legal inside an `INSERT`, so anywhere else it is refused
/// rather than emitted. PostgreSQL agrees: it answers "DEFAULT is not allowed
/// in this context".
#[test]
fn default_outside_an_insert_is_rejected() {
    let error = translate_err(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT);
         SELECT * FROM (VALUES (1, DEFAULT)) AS v;",
    );

    assert!(
        error.to_uppercase().contains("DEFAULT"),
        "expected the error to name the keyword, got {error}"
    );
}

/// Guards the fix from disturbing an ordinary multi-row insert.
#[test]
fn values_without_a_default_still_insert() {
    let mut conn = apply(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT);
         INSERT INTO t (id, s) VALUES (1, 'a'), (2, 'b');",
    );

    assert_eq!(schema::t::table.count().get_result::<i64>(&mut conn).expect("count"), 2);
}
