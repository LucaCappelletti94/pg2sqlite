//! F5: `FOREIGN KEY ... MATCH FULL` was emitted verbatim and never enforced.
//!
//! PostgreSQL's MATCH FULL says a composite foreign key is either wholly NULL,
//! in which case the row is exempt, or wholly non-NULL, in which case it must
//! match. Mixing the two is an error. SQLite parses the MATCH clause and then
//! always behaves as MATCH SIMPLE, where a single NULL anywhere exempts the
//! row, so the mixed row was accepted in silence.
//!
//! A composite MATCH FULL is exactly emulable: keep the foreign key, which
//! already gives the wholly-NULL and wholly-non-NULL cases, and add a table
//! CHECK that refuses the mixture. A single-column MATCH FULL needs nothing,
//! since with one column the two readings coincide.
//!
//! Every expectation was read off PostgreSQL 17 before the fix. The emitted
//! statements are executed as text because that text is the artifact under
//! test; reads go through the typed diesel DSL.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        child (rowid) {
            rowid -> Integer,
            x -> Nullable<Integer>,
            y -> Nullable<Integer>,
        }
    }
}

const PARENT: &str = "CREATE TABLE parent (a INT, b INT, PRIMARY KEY (a, b));
     INSERT INTO parent (a, b) VALUES (1, 1);";

/// The composite foreign key under test, spelled with the given MATCH clause.
fn composite(match_clause: &str) -> String {
    format!(
        "{PARENT}
         CREATE TABLE child (x INT, y INT, FOREIGN KEY (x, y) REFERENCES parent (a, b) {match_clause});"
    )
}

fn translate(pg: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
}

fn translate_err(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("this shape has no SQLite form")
        .to_string()
}

/// Runs every emitted statement with foreign keys switched on, panicking if
/// any of them fails.
fn apply(pg: &str) -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection).expect("pragma");
    for statement in translate(pg) {
        diesel::sql_query(&statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted statement failed: {statement}: {error}"));
    }
    connection
}

/// The first execution failure, for the rows PostgreSQL refuses.
fn apply_err(pg: &str) -> diesel::result::Error {
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection).expect("pragma");
    for statement in translate(pg) {
        if let Err(error) = diesel::sql_query(&statement).execute(&mut connection) {
            return error;
        }
    }
    panic!("every emitted statement succeeded, expected one to fail");
}

fn stored(connection: &mut SqliteConnection) -> Vec<(Option<i32>, Option<i32>)> {
    schema::child::table
        .select((schema::child::x, schema::child::y))
        .order(schema::child::rowid)
        .load(connection)
        .expect("load")
}

// ---------------------------------------------------------------------------
// Composite MATCH FULL
// ---------------------------------------------------------------------------

/// The item's own trigger. PostgreSQL reports `MATCH FULL does not allow
/// mixing of null and nonnull key values`, and the emitted schema accepted the
/// row.
#[test]
fn a_mixed_null_row_is_refused() {
    let error = apply_err(&format!(
        "{}
         INSERT INTO child (x, y) VALUES (NULL, 1);",
        composite("MATCH FULL")
    ));
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "the mixture must be refused: {error}"
    );
}

/// A wholly NULL row is exempt, which is the half of MATCH FULL the foreign
/// key already gave and the guard must not take away.
#[test]
fn an_all_null_row_is_allowed() {
    let mut connection = apply(&format!(
        "{}
         INSERT INTO child (x, y) VALUES (NULL, NULL);",
        composite("MATCH FULL")
    ));
    assert_eq!(stored(&mut connection), vec![(None, None)]);
}

/// A wholly non-NULL row that matches is allowed.
#[test]
fn a_matching_row_is_allowed() {
    let mut connection = apply(&format!(
        "{}
         INSERT INTO child (x, y) VALUES (1, 1);",
        composite("MATCH FULL")
    ));
    assert_eq!(stored(&mut connection), vec![(Some(1), Some(1))]);
}

/// The reference itself is still enforced, so the guard has not replaced the
/// foreign key with a weaker check.
#[test]
fn a_non_matching_row_is_still_refused_by_the_reference() {
    let error = apply_err(&format!(
        "{}
         INSERT INTO child (x, y) VALUES (2, 2);",
        composite("MATCH FULL")
    ));
    assert!(
        error.to_string().contains("FOREIGN KEY constraint failed"),
        "the reference must still be enforced: {error}"
    );
}

/// An UPDATE can create the mixture just as an INSERT can, and a table CHECK
/// covers both.
#[test]
fn an_update_into_the_mixture_is_refused() {
    let error = apply_err(&format!(
        "{}
         INSERT INTO child (x, y) VALUES (NULL, NULL);
         UPDATE child SET y = 1;",
        composite("MATCH FULL")
    ));
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "an update into the mixture must be refused: {error}"
    );
}

/// More than two columns take the same guard, generalized over all of them.
#[test]
fn the_guard_generalizes_past_two_columns() {
    let three = "CREATE TABLE p3 (a INT, b INT, c INT, PRIMARY KEY (a, b, c));
         CREATE TABLE k3 (x INT, y INT, z INT,
             CONSTRAINT k3_fk FOREIGN KEY (x, y, z) REFERENCES p3 (a, b, c) MATCH FULL);";
    let error = apply_err(&format!("{three}\nINSERT INTO k3 (x, y, z) VALUES (1, NULL, 1);"));
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "a partially NULL three column key must be refused: {error}"
    );
}

/// The guard is named, because an anonymous one reports its whole expression
/// and says nothing about which constraint it stands for.
#[test]
fn the_guard_is_named_after_the_constraint() {
    let named = translate(&format!(
        "{PARENT}
         CREATE TABLE child (x INT, y INT,
             CONSTRAINT child_parent_fk FOREIGN KEY (x, y) REFERENCES parent (a, b) MATCH FULL);"
    ))
    .join("\n");
    assert!(
        named.contains("CONSTRAINT child_parent_fk_match_full CHECK"),
        "a named foreign key names its guard after itself: {named}"
    );

    let anonymous = translate(&composite("MATCH FULL")).join("\n");
    assert!(
        anonymous.contains("CONSTRAINT x_y_match_full CHECK"),
        "an anonymous foreign key names its guard after the columns: {anonymous}"
    );
}

// ---------------------------------------------------------------------------
// The shapes that need no guard
// ---------------------------------------------------------------------------

/// MATCH SIMPLE is what SQLite already does, so a mixed NULL row stays
/// allowed and no guard appears.
#[test]
fn match_simple_still_exempts_a_mixed_null_row() {
    let emitted = translate(&composite("MATCH SIMPLE")).join("\n");
    assert!(!emitted.contains("CHECK"), "MATCH SIMPLE needs no guard: {emitted}");

    let mut connection = apply(&format!(
        "{}
         INSERT INTO child (x, y) VALUES (NULL, 1);",
        composite("MATCH SIMPLE")
    ));
    assert_eq!(stored(&mut connection), vec![(None, Some(1))]);
}

/// The default is MATCH SIMPLE, so a foreign key with no MATCH clause is
/// untouched.
#[test]
fn a_foreign_key_without_a_match_clause_is_untouched() {
    let emitted = translate(&composite("")).join("\n");
    assert!(!emitted.contains("CHECK"), "no MATCH clause means no guard: {emitted}");
}

/// With one column the two readings coincide, so MATCH FULL there gets no
/// guard and a NULL is still exempt.
#[test]
fn a_single_column_match_full_needs_no_guard() {
    let single = "CREATE TABLE p1 (a INT PRIMARY KEY);
         CREATE TABLE c1 (x INT, FOREIGN KEY (x) REFERENCES p1 (a) MATCH FULL);";
    let emitted = translate(single).join("\n");
    assert!(!emitted.contains("CHECK"), "one column needs no guard: {emitted}");
    apply(&format!("{single}\nINSERT INTO c1 (x) VALUES (NULL);"));
}

/// The column-level spelling is single-column by construction and likewise
/// needs no guard.
#[test]
fn a_column_level_match_full_needs_no_guard() {
    let column_level = "CREATE TABLE p1 (a INT PRIMARY KEY);
         CREATE TABLE c1 (x INT REFERENCES p1 (a) MATCH FULL);";
    let emitted = translate(column_level).join("\n");
    assert!(!emitted.contains("CHECK"), "one column needs no guard: {emitted}");
    apply(&format!("{column_level}\nINSERT INTO c1 (x) VALUES (NULL);"));
}

// ---------------------------------------------------------------------------
// MATCH PARTIAL
// ---------------------------------------------------------------------------

/// PostgreSQL refuses MATCH PARTIAL itself, with `MATCH PARTIAL not yet
/// implemented`, so no valid PostgreSQL input carries it and emitting a clause
/// SQLite ignores would claim an enforcement nobody has implemented.
#[test]
fn match_partial_is_refused() {
    let error = translate_err(&composite("MATCH PARTIAL"));
    assert!(error.contains("PARTIAL"), "the refusal must name the clause: {error}");
}

/// Same at the column level.
#[test]
fn a_column_level_match_partial_is_refused() {
    let error = translate_err(
        "CREATE TABLE p1 (a INT PRIMARY KEY);
         CREATE TABLE c1 (x INT REFERENCES p1 (a) MATCH PARTIAL);",
    );
    assert!(error.contains("PARTIAL"), "the refusal must name the clause: {error}");
}
