//! `greatest` and `least` ignore NULL arguments in PostgreSQL, where SQLite's
//! scalar `max` and `min` return NULL as soon as one argument is NULL.
//!
//! Measured on PostgreSQL 16: `greatest(1, NULL, 3)` is 3, `least(1, NULL, 3)`
//! is 1, `least('a', NULL)` is `a`, and both are NULL only when every argument
//! is. Measured on SQLite 3.51.1: `max(1, NULL, 3)` is NULL.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::Pg2SqliteOptions;
use run_translated_helper::run_translated_with;

/// Rows chosen so that the NULL sits in a different position each time, since a
/// rewrite that only skips a trailing or leading NULL would pass otherwise.
const TABLE: &str = "CREATE TABLE t (k INT PRIMARY KEY, a INT, b INT, c INT);
     INSERT INTO t VALUES (1, 2, NULL, 7), (2, NULL, NULL, NULL), (3, 5, 3, NULL);";

fn probe(expression: &str) -> Vec<Option<String>> {
    run_translated_with(
        &format!("{TABLE} SELECT {expression} FROM t ORDER BY k;"),
        &Pg2SqliteOptions::default(),
    )
}

#[test]
fn greatest_ignores_null_arguments() {
    assert_eq!(
        probe("greatest(a, b, c)"),
        vec![Some("7".to_string()), None, Some("5".to_string())]
    );
}

#[test]
fn least_ignores_null_arguments() {
    assert_eq!(probe("least(a, b, c)"), vec![Some("2".to_string()), None, Some("3".to_string())]);
}

/// Two arguments is the common shape and takes a different rotation than three.
#[test]
fn two_argument_greatest_and_least_ignore_nulls() {
    assert_eq!(probe("greatest(a, b)"), vec![Some("2".to_string()), None, Some("5".to_string())]);
    assert_eq!(probe("least(a, b)"), vec![Some("2".to_string()), None, Some("3".to_string())]);
}

/// PostgreSQL accepts a single argument, and SQLite's one-argument `max` is the
/// AGGREGATE, which would collapse the rows instead of returning one per row.
#[test]
fn single_argument_greatest_does_not_become_an_aggregate() {
    assert_eq!(
        probe("greatest(a)"),
        vec![Some("2".to_string()), None, Some("5".to_string())],
        "one row per input row, not one row for the table"
    );
}

/// Text comparison, to catch a rewrite that only works for numbers.
#[test]
fn least_over_text_ignores_nulls() {
    let rows = run_translated_with(
        "CREATE TABLE s (k INT PRIMARY KEY, x TEXT, y TEXT);
         INSERT INTO s VALUES (1, 'a', NULL);
         SELECT least(x, y) FROM s;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("a".to_string())]);
}

/// A CHECK constraint accepts no subquery, so the rewrite has to stay an
/// ordinary expression. This fails to translate, rather than returning a wrong
/// answer, if that ever stops holding.
#[test]
fn greatest_is_usable_where_a_subquery_is_not() {
    let rows = run_translated_with(
        "CREATE TABLE c (a INT, b INT, CHECK (greatest(a, b) > 0));
         INSERT INTO c VALUES (5, NULL);
         SELECT a FROM c;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("5".to_string())]);
}
