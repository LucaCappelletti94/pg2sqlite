//! `WITH ORDINALITY` must not reach the output, since SQLite has no such
//! clause: `SELECT * FROM t WITH ORDINALITY` is `near "ORDINALITY": syntax
//! error`.
//!
//! Two of the three table factors that carry the flag copied it through
//! unguarded. The third, `UNNEST`, was already handled, because forward
//! translation lowers it onto `json_each` and builds the ordinality column
//! itself.
//!
//! Worth knowing about the two that were not: `FROM <table> WITH ORDINALITY` is
//! not PostgreSQL at all, verified rejected by PostgreSQL 16 with `syntax error
//! at or near "WITH"`, since the clause is only allowed on a set-returning
//! function. It is reachable here only because `sqlparser` is more permissive,
//! so it is refused for the same reason the MySQL rename spellings are.
//! `FROM <function> WITH ORDINALITY` IS valid PostgreSQL, and it is refused
//! because there is nothing to attach the ordinality to: a set-returning
//! function in `FROM` has no SQLite form either.

use diesel::{
    QueryableByName, RunQueryDsl,
    prelude::*,
    sql_types::{Integer, Text},
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// One row of the translated `UNNEST` query. The query under test is the
/// artifact, so it is run as generated text rather than through the DSL.
#[derive(QueryableByName)]
struct Numbered {
    #[diesel(sql_type = Text)]
    v: String,
    #[diesel(sql_type = Integer)]
    ordinality: i32,
}
fn translate(pg: &str) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    Pg2Sqlite::default().sql(pg)?.translate_to_sql(&Pg2SqliteOptions::default())
}

/// Arrays need a declared representation before `UNNEST` can be lowered, so the
/// one test that uses an array literal opts in.
fn translate_with_json_arrays(pg: &str) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    let options = Pg2SqliteOptions::default()
        .with_array_representation(pg2sqlite::prelude::ArrayRepresentation::Json);
    Pg2Sqlite::default().sql(pg)?.translate_to_sql(&options)
}

fn translate_err(pg: &str) -> String {
    translate(pg).expect_err("expected a translation error").to_string()
}

const BASE: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT);";

/// The table factor. Not PostgreSQL, and invalid SQLite, so it is refused
/// rather than emitted.
#[test]
fn with_ordinality_on_a_table_is_rejected() {
    let error = translate_err(&format!("{BASE} SELECT * FROM t WITH ORDINALITY;"));
    assert!(
        error.to_uppercase().contains("ORDINALITY"),
        "expected the error to name the clause, got {error}"
    );
}

/// The function factor. Valid PostgreSQL, still refused, because the function
/// itself has no SQLite form to hang an ordinality column on.
#[test]
fn with_ordinality_on_a_function_is_rejected() {
    let error = translate_err(&format!(
        "{BASE} SELECT * FROM json_array_elements('[1,2]') WITH ORDINALITY;"
    ));
    assert!(
        error.to_uppercase().contains("ORDINALITY"),
        "the clause must be what is reported, not something else about the function: {error}"
    );
}

/// Guards the fix: `UNNEST ... WITH ORDINALITY` is lowered onto `json_each` and
/// must keep translating. It passed before the change and must keep passing.
///
/// Asserted by executing rather than by looking for the keyword, which would be
/// misleading twice over: the emitted SQL legitimately contains the word as a
/// column ALIAS (`key + 1 AS ordinality`), and a passing substring check proves
/// nothing about whether the numbering is right.
#[test]
fn unnest_with_ordinality_still_numbers_the_rows() {
    let translated = translate_with_json_arrays(
        "SELECT v, ordinality FROM unnest(ARRAY['a', 'b']) WITH ORDINALITY AS v;",
    )
    .expect("UNNEST WITH ORDINALITY must keep translating");

    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    let sql = translated.join("; ");
    let numbered: Vec<Numbered> = diesel::sql_query(&sql)
        .load(&mut conn)
        .unwrap_or_else(|error| panic!("the emitted query must execute: {sql}: {error}"));

    assert_eq!(
        numbered.iter().map(|row| (row.v.as_str(), row.ordinality)).collect::<Vec<_>>(),
        vec![("a", 1), ("b", 2)]
    );
}

/// Guards the fix from refusing an ordinary table reference.
#[test]
fn a_plain_table_factor_still_translates() {
    translate(&format!("{BASE} SELECT id FROM t;")).expect("a plain table must still translate");
}
