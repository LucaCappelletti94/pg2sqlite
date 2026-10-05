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

/// One text line read back through a raw query, for the tables below that no
/// diesel schema declares.
#[derive(QueryableByName)]
struct Line {
    #[diesel(sql_type = diesel::sql_types::Text)]
    line: String,
}

fn read_line(conn: &mut SqliteConnection, sql: &str) -> String {
    diesel::sql_query(sql).get_result::<Line>(conn).expect("read").line
}

/// PostgreSQL accepts `DEFAULT` in an UPDATE assignment too and stores the
/// declared default, measured on PostgreSQL 16. SQLite has no form of it, so
/// the keyword has to be substituted here exactly as in a VALUES row.
#[test]
fn update_set_default_stores_the_declared_default() {
    let mut conn = apply(
        "CREATE TABLE u (id INTEGER PRIMARY KEY, c INTEGER DEFAULT 9);
         INSERT INTO u (id, c) VALUES (1, 7);
         UPDATE u SET c = DEFAULT WHERE id = 1;",
    );
    assert_eq!(read_line(&mut conn, "SELECT CAST(c AS TEXT) AS line FROM u"), "9");
}

/// With no declared default PostgreSQL stores NULL, measured on 16.
#[test]
fn update_set_default_without_a_declared_default_stores_null() {
    let mut conn = apply(
        "CREATE TABLE u (id INTEGER PRIMARY KEY, a INTEGER);
         INSERT INTO u (id, a) VALUES (1, 5);
         UPDATE u SET a = DEFAULT WHERE id = 1;",
    );
    assert_eq!(
        read_line(&mut conn, "SELECT coalesce(CAST(a AS TEXT), '<null>') AS line FROM u"),
        "<null>"
    );
}

/// The tuple spelling substitutes per position, measured on PostgreSQL 16:
/// `SET (c, d) = (DEFAULT, 40)` stores the declared 9 beside the given 40.
#[test]
fn a_tuple_update_substitutes_each_default() {
    let mut conn = apply(
        "CREATE TABLE u (id INTEGER PRIMARY KEY, c INTEGER DEFAULT 9, d INTEGER DEFAULT 4);
         INSERT INTO u (id, c, d) VALUES (1, 7, 8);
         UPDATE u SET (c, d) = (DEFAULT, 40) WHERE id = 1;",
    );
    assert_eq!(read_line(&mut conn, "SELECT c || '|' || d AS line FROM u"), "9|40");
}

/// The upsert's assignment list is the same write through a different door,
/// measured on PostgreSQL 16.
#[test]
fn an_upsert_do_update_substitutes_the_default() {
    let mut conn = apply(
        "CREATE TABLE u (id INTEGER PRIMARY KEY, c INTEGER DEFAULT 9);
         INSERT INTO u (id, c) VALUES (1, 7);
         INSERT INTO u (id, c) VALUES (1, 7) ON CONFLICT (id) DO UPDATE SET c = DEFAULT;",
    );
    assert_eq!(read_line(&mut conn, "SELECT CAST(c AS TEXT) AS line FROM u"), "9");
}

/// The substituted default is a raw PostgreSQL literal and runs through the
/// ordinary pipeline, so a scaled NUMERIC default lands in minor units.
#[test]
fn update_set_default_on_a_scaled_numeric_stores_minor_units() {
    let mut conn = apply(
        "CREATE TABLE u (id INTEGER PRIMARY KEY, price NUMERIC(10,2) DEFAULT 1.50);
         INSERT INTO u (id, price) VALUES (1, 9.99);
         UPDATE u SET price = DEFAULT WHERE id = 1;",
    );
    assert_eq!(read_line(&mut conn, "SELECT CAST(price AS TEXT) AS line FROM u"), "150");
}

/// The same refusal the INSERT door gives: the statement could only fail,
/// since the column is NOT NULL and declares nothing to fall back to.
/// PostgreSQL fails at run time instead, which is the one deliberate
/// divergence, recorded at the INSERT arm it mirrors.
#[test]
fn update_set_default_on_not_null_without_a_default_is_refused() {
    let error = translate_err(
        "CREATE TABLE u (id INTEGER PRIMARY KEY, b INTEGER NOT NULL);
         UPDATE u SET b = DEFAULT WHERE id = 1;",
    );
    assert!(
        error.contains("declares no default"),
        "expected the INSERT door's wording, got {error}"
    );
}
