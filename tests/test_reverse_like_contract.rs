//! F24: the connection setting the reverse direction assumes.
//!
//! SQLite's LIKE ignores letter case unless the connection says otherwise.
//! PostgreSQL's never does. The reverse direction hands a plain LIKE back as a
//! plain LIKE, which is right only when the SQL it was given ran with
//! `PRAGMA case_sensitive_like = true`, the setting the forward direction
//! writes into its own script.
//!
//! That promise is documented on the reverse entry points. These tests hold the
//! facts the promise rests on, so a future change to any of them fails here
//! rather than quietly making the documentation false:
//!
//! - the two readings of a plain LIKE really do differ, and the pragma really
//!   does close the gap
//! - the forward direction really does emit the pragma for a script with a LIKE
//! - a plain LIKE really is handed back unchanged
//!
//! Turning a plain LIKE into an ILIKE was considered and measured: SQLite folds
//! the ASCII letters only, so it would introduce a non-ASCII divergence rather
//! than remove one. There is a test for that too.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;
use sql_traits::structs::ParserDB;

const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);";

fn schema() -> ParserDB {
    Pg2Sqlite::default().sql(SCHEMA).expect("parse").build_schema().expect("build")
}

fn reverse(sqlite: &str) -> String {
    Pg2Sqlite::default()
        .reverse_sql(sqlite, &schema(), &Pg2SqliteOptions::default())
        .expect("reverse")
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// Answers one boolean expression on a fresh SQLite connection.
fn sqlite_says(pragma: Option<&str>, expression: &str) -> i64 {
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    if let Some(pragma) = pragma {
        connection.execute_batch(pragma).expect("pragma");
    }
    connection.query_row(&format!("SELECT {expression}"), [], |row| row.get(0)).expect("expression")
}

/// Without the pragma the two engines disagree, which is why the contract
/// exists at all.
#[test]
fn a_plain_like_ignores_case_in_sqlite_by_default() {
    assert_eq!(sqlite_says(None, "'A' LIKE 'a'"), 1);
}

/// With it they agree, which is what makes handing the LIKE back unchanged
/// correct.
#[test]
fn the_pragma_makes_a_plain_like_case_sensitive() {
    assert_eq!(sqlite_says(Some("PRAGMA case_sensitive_like = true;"), "'A' LIKE 'a'"), 0);
}

/// The folding is ASCII-only, which is what rules out reversing a plain LIKE
/// into an ILIKE: PostgreSQL's ILIKE would answer true here.
#[test]
fn the_default_folding_does_not_reach_beyond_ascii() {
    assert_eq!(sqlite_says(None, "'\u{c4}' LIKE '\u{e4}'"), 0);
}

/// The forward direction writes the pragma the contract names, so a caller
/// applying its script starts on a connection that satisfies the promise.
#[test]
fn the_forward_direction_emits_the_pragma_for_a_like() {
    let statements = Pg2Sqlite::default()
        .sql(&format!("{SCHEMA}\nSELECT s FROM t WHERE s LIKE 'a%';"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate");
    assert!(
        statements.iter().any(|s| s.contains("case_sensitive_like")),
        "the pragma is what the contract points at: {statements:?}"
    );
}

/// And only for one, so a schema carrying no LIKE hands the caller nothing,
/// which is the failure the documentation names.
#[test]
fn a_script_without_a_like_carries_no_pragma() {
    let statements = Pg2Sqlite::default()
        .sql(SCHEMA)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate");
    assert!(!statements.iter().any(|s| s.contains("case_sensitive_like")), "{statements:?}");
}

#[test]
fn a_plain_like_comes_back_as_a_plain_like() {
    let pg = reverse("SELECT s FROM t WHERE s LIKE 'a%'");
    assert!(pg.contains("LIKE 'a%'"), "{pg}");
    assert!(!pg.contains("ILIKE"), "{pg}");
}

#[test]
fn a_negated_plain_like_comes_back_negated() {
    let pg = reverse("SELECT s FROM t WHERE s NOT LIKE 'a%'");
    assert!(pg.contains("NOT LIKE 'a%'"), "{pg}");
}

/// The lowered pair the forward direction emits for an ILIKE still restores,
/// so the contract did not swallow the one case that is unambiguous.
#[test]
fn the_lowered_pair_still_restores_to_ilike() {
    let pg = reverse("SELECT s FROM t WHERE lower(s) LIKE lower('a%')");
    assert!(pg.contains("ILIKE"), "{pg}");
}
