//! `string_agg` and its two hard edges.
//!
//! Measured on PostgreSQL 16 over `(3,'c')`, `(1,'a')`, `(2,'b')`, `(4,'a')`:
//! `string_agg(s, ',' ORDER BY id)` is `a,b,c,a` and `string_agg(DISTINCT s,
//! ',')` is `a,b,c`.
//!
//! Measured on SQLite 3.51.1: `group_concat(s, ',' ORDER BY id)` gives the same
//! `a,b,c,a`, since ordering inside an aggregate arrived in 3.44.0 and the
//! declared floor is 3.46.0. But `group_concat(DISTINCT s, ',')` is
//! `DISTINCT aggregates must have exactly one argument` in every version, so
//! the separator has to go, and the one it uses when it does is a comma.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
     INSERT INTO t VALUES (3, 'c'), (1, 'a'), (2, 'b'), (4, 'a');";

fn evaluate(expression: &str) -> Option<String> {
    run_translated_with(
        &format!("{TABLE} SELECT {expression} FROM t;"),
        &Pg2SqliteOptions::default(),
    )
    .into_iter()
    .next()
    .expect("one row")
}

/// Ordering inside the aggregate is the whole point of this spelling, and it
/// is what the 3.46.0 floor buys.
#[test]
fn an_ordered_aggregate_keeps_its_order() {
    assert_eq!(evaluate("string_agg(s, ',' ORDER BY id)"), Some("a,b,c,a".to_string()));
    assert_eq!(evaluate("string_agg(s, '-' ORDER BY id DESC)"), Some("a-c-b-a".to_string()));
}

/// SQLite takes no separator beside DISTINCT, and the one it uses is a comma,
/// so this spelling translates exactly.
#[test]
fn a_distinct_aggregate_with_a_comma_separator_translates() {
    assert_eq!(evaluate("string_agg(DISTINCT s, ',' ORDER BY s)"), Some("a,b,c".to_string()));
}

/// Any other separator has no faithful form: rewriting it as a replace over the
/// comma joined result would corrupt any value containing a comma, so it is
/// refused rather than silently mangled.
#[test]
fn a_distinct_aggregate_with_another_separator_is_refused() {
    let error = Pg2Sqlite::default()
        .sql(&format!("{TABLE} SELECT string_agg(DISTINCT s, '-') FROM t;"))
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("SQLite takes no separator beside DISTINCT")
        .to_string();
    assert!(error.contains("DISTINCT"), "the error must name the clause, got: {error}");
}

/// The plain spelling was already right and must stay so. Neither database
/// promises an order without `ORDER BY`, so only the multiset is asserted.
#[test]
fn a_plain_aggregate_keeps_every_value() {
    let joined = evaluate("string_agg(s, ',')").expect("a value");
    let mut parts: Vec<&str> = joined.split(',').collect();
    parts.sort_unstable();
    assert_eq!(parts, ["a", "a", "b", "c"]);
}
