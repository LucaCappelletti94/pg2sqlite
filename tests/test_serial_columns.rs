//! F6: a serial column that is not the primary key had no value source.
//!
//! PostgreSQL's `SERIAL` is shorthand for `integer NOT NULL DEFAULT
//! nextval('...')`, verified by reading `information_schema.columns` back on
//! PostgreSQL 17. SQLite auto-assigns only through the `INTEGER PRIMARY KEY`
//! rowid alias, so a serial anywhere else had nothing to supply a value and
//! every row stored NULL in silence.
//!
//! The crate already refused both other spellings of the same concept,
//! `GENERATED AS IDENTITY` off the primary key and the literal `DEFAULT
//! nextval('...')`, so the shorthand is now refused with them.
//!
//! What must keep working is every shape where SQLite really does auto-assign,
//! and that includes a single-column `PRIMARY KEY (n)` written as a table
//! constraint, which is a rowid alias just as the inline spelling is. Measured
//! on SQLite 3.46.0 and on PostgreSQL 17, both answer 1 and 2 for two rows.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        s (n) {
            n -> Integer,
            tag -> Nullable<Text>,
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

fn refusal(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("a serial with no value source cannot be translated")
        .to_string()
}

/// Applies the translation and reads back what the auto-assigned column holds.
fn assigned(pg: &str) -> Vec<(i32, Option<String>)> {
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    for statement in translate(pg) {
        diesel::sql_query(&statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted statement failed: {statement}: {error}"));
    }
    schema::s::table
        .select((schema::s::n, schema::s::tag))
        .order(schema::s::n)
        .load(&mut connection)
        .expect("load")
}

// ---------------------------------------------------------------------------
// Refused: no value source
// ---------------------------------------------------------------------------

/// The item's own trigger. Two rows stored NULL where PostgreSQL stores 1
/// and 2.
#[test]
fn a_serial_that_is_only_unique_is_refused() {
    let error = refusal("CREATE TABLE s (n BIGSERIAL UNIQUE, tag TEXT);");
    assert!(error.contains("'n'"), "the refusal must name the column: {error}");
    assert!(
        error.to_uppercase().contains("PRIMARY KEY"),
        "the refusal must say what does work: {error}"
    );
}

/// A bare serial with no constraint at all is the same problem.
#[test]
fn a_bare_serial_is_refused() {
    let error = refusal("CREATE TABLE s (n SERIAL, tag TEXT);");
    assert!(error.to_uppercase().contains("PRIMARY KEY"), "{error}");
}

/// Every width of the shorthand, since they all map onto the same integer.
#[test]
fn every_serial_width_is_refused() {
    for spelling in ["SMALLSERIAL", "SERIAL", "BIGSERIAL"] {
        let error = refusal(&format!("CREATE TABLE s (n {spelling}, tag TEXT);"));
        assert!(error.to_uppercase().contains("PRIMARY KEY"), "{spelling}: {error}");
    }
}

/// A composite primary key is not a rowid alias, so there is still no value
/// source. This one used to translate and then fail at insert time on the
/// NOT NULL that SQLite adds to a primary key column, so the change moves the
/// complaint earlier and makes it say why.
#[test]
fn a_serial_inside_a_composite_primary_key_is_refused() {
    let error = refusal("CREATE TABLE s (n SERIAL, m INT, PRIMARY KEY (n, m));");
    assert!(error.to_uppercase().contains("PRIMARY KEY"), "{error}");
}

/// `ALTER TABLE ADD COLUMN` cannot add a primary key in SQLite, so a serial
/// added that way can never be a rowid alias.
#[test]
fn a_serial_added_by_alter_table_is_refused() {
    let error = refusal(
        "CREATE TABLE s (id INT PRIMARY KEY);
         ALTER TABLE s ADD COLUMN n SERIAL;",
    );
    assert!(error.to_uppercase().contains("PRIMARY KEY"), "{error}");
}

// ---------------------------------------------------------------------------
// Kept: SQLite really does auto-assign
// ---------------------------------------------------------------------------

/// The inline spelling is a rowid alias and keeps working.
#[test]
fn a_serial_primary_key_still_auto_assigns() {
    let rows = assigned(
        "CREATE TABLE s (n SERIAL PRIMARY KEY, tag TEXT);
         INSERT INTO s (tag) VALUES ('a'), ('b');",
    );
    assert_eq!(rows, vec![(1, Some("a".to_owned())), (2, Some("b".to_owned()))]);
}

/// The table-constraint spelling of the same thing is also a rowid alias, so
/// it must not be swept up by the refusal.
#[test]
fn a_table_level_single_column_primary_key_still_auto_assigns() {
    let rows = assigned(
        "CREATE TABLE s (n SERIAL, tag TEXT, PRIMARY KEY (n));
         INSERT INTO s (tag) VALUES ('a'), ('b');",
    );
    assert_eq!(rows, vec![(1, Some("a".to_owned())), (2, Some("b".to_owned()))]);
}

// ---------------------------------------------------------------------------
// The identity spelling, which shares the check
// ---------------------------------------------------------------------------

/// Identity off the primary key was already refused and stays refused.
#[test]
fn identity_off_the_primary_key_is_still_refused() {
    let error =
        refusal("CREATE TABLE s (n INT GENERATED BY DEFAULT AS IDENTITY UNIQUE, tag TEXT);");
    assert!(error.contains("IDENTITY"), "{error}");
}

/// Identity with a table-level single-column primary key was refused too,
/// wrongly: the SQLite it would emit is a rowid alias and auto-assigns. Both
/// paths now share one notion of the alias, so this works.
#[test]
fn identity_with_a_table_level_primary_key_auto_assigns() {
    let rows = assigned(
        "CREATE TABLE s (n INT GENERATED BY DEFAULT AS IDENTITY, tag TEXT, PRIMARY KEY (n));
         INSERT INTO s (tag) VALUES ('a'), ('b');",
    );
    assert_eq!(rows, vec![(1, Some("a".to_owned())), (2, Some("b".to_owned()))]);
}

/// The inline identity spelling keeps working.
#[test]
fn identity_as_the_primary_key_still_auto_assigns() {
    let rows = assigned(
        "CREATE TABLE s (n INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, tag TEXT);
         INSERT INTO s (tag) VALUES ('a'), ('b');",
    );
    assert_eq!(rows, vec![(1, Some("a".to_owned())), (2, Some("b".to_owned()))]);
}
