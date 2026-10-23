//! PostgreSQL reads the clauses around a parenthesized query as clauses of
//! that same query, while SQLite has no parenthesized query at all.
//!
//! Every expected value is what PostgreSQL 18.6 answers, measured.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

const ROWS: &str = "CREATE TABLE q (id int PRIMARY KEY, a int);
     INSERT INTO q VALUES (1, 1), (2, 2), (3, 1), (4, NULL);";

/// The rows `pg` answers.
fn answer(pg: &str) -> Vec<Option<i64>> {
    run_translated_with(pg, &Pg2SqliteOptions::default())
        .into_iter()
        .map(|value| value.map(|text| text.parse().expect("an integer")))
        .collect()
}

/// The refusal `pg` earns.
fn refusal(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("expected a refusal")
        .to_string()
}

/// A parenthesized statement is the query inside it.
#[test]
fn a_parenthesized_statement_runs() {
    assert_eq!(answer("(SELECT 0 AS n);"), [Some(0)]);
    assert_eq!(answer(&format!("{ROWS} ((SELECT a FROM q WHERE id = 2));")), [Some(2)]);
}

/// Clauses outside the parentheses join the ones inside, ordering before
/// limiting.
#[test]
fn outer_clauses_join_the_inner_query() {
    assert_eq!(
        answer(&format!("{ROWS} (SELECT a FROM q ORDER BY id LIMIT 2) OFFSET 1;")),
        [Some(2), Some(1)]
    );
    assert_eq!(
        answer(&format!("{ROWS} (SELECT a FROM q LIMIT 2) ORDER BY a;")),
        [Some(1), Some(1)]
    );
    assert_eq!(
        answer(&format!("{ROWS} ((SELECT a FROM q ORDER BY id)) LIMIT 3 OFFSET 1;")),
        [Some(2), Some(1), None]
    );
    assert_eq!(
        answer(&format!(
            "{ROWS} WITH x AS (SELECT a FROM q) (SELECT a FROM x ORDER BY a LIMIT 1);"
        )),
        [Some(1)]
    );
}

/// A derived table holding a parenthesized query with outer clauses answers
/// the same rows.
#[test]
fn a_derived_table_joins_its_clauses_too() {
    assert_eq!(
        answer(&format!("{ROWS} SELECT * FROM ((SELECT a FROM q ORDER BY id) LIMIT 1) AS s;")),
        [Some(1)]
    );
}

/// A clause given both inside and outside the parentheses is refused, as
/// PostgreSQL refuses it.
#[test]
fn a_clause_given_twice_is_refused() {
    for (query, clause) in [
        ("(SELECT a FROM q LIMIT 2) LIMIT 1", "LIMIT"),
        ("(SELECT a FROM q LIMIT 2) FETCH FIRST 1 ROW ONLY", "LIMIT"),
        ("(SELECT a FROM q FETCH FIRST 2 ROWS ONLY) LIMIT 1", "LIMIT"),
        ("(SELECT a FROM q OFFSET 1) OFFSET 2", "OFFSET"),
        ("(SELECT a FROM q ORDER BY a) ORDER BY a DESC", "ORDER BY"),
        ("WITH x AS (SELECT 1) (WITH y AS (SELECT 2) SELECT 1)", "WITH"),
    ] {
        let message = refusal(&format!("{ROWS} {query};"));
        assert!(message.contains(&format!("multiple {clause} clauses")), "{query}: {message}");
    }
}
