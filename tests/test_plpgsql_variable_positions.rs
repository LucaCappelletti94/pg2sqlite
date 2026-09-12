//! Every position a PL/pgSQL variable can be read from, proved by executing
//! the emitted trigger.
//!
//! A variable is bound once and read as a value, so the position it is read
//! from cannot change the answer. The translator used to substitute only a
//! projection item and a `WHERE` predicate, and every other position emitted a
//! bare identifier that SQLite rejects with `no such column`, which makes
//! `CREATE TRIGGER` itself fail. Each test here fires the emitted trigger and
//! reads the rows it produced.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

/// Translates a trigger function with `body`, applies the emitted script,
/// fires the trigger, and returns `log.total` in ascending order.
///
/// # Panics
///
/// Panics when translation fails or when an emitted statement will not
/// execute, which is the failure this file exists to catch.
fn totals_after_firing(body: &str) -> Vec<i64> {
    totals_after_declaring("v_n INTEGER := 3;", body)
}

/// As [`totals_after_firing`], with the `DECLARE` section spelled out.
///
/// # Panics
///
/// Panics when translation fails or when an emitted statement will not
/// execute.
fn totals_after_declaring(declaration: &str, body: &str) -> Vec<i64> {
    let pg = format!(
        "
CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER);
CREATE TABLE log (id INTEGER PRIMARY KEY, total INTEGER);
CREATE TABLE src (id INTEGER PRIMARY KEY, v INTEGER);
CREATE OR REPLACE FUNCTION f() RETURNS TRIGGER AS $$
DECLARE
    {declaration}
BEGIN
    {body}
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER tr AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();
"
    );

    let statements = Pg2Sqlite::default()
        .sql(&pg)
        .expect("fixture parses")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("fixture translates");

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted statement failed: {error}\n{statement}"));
    }
    connection
        .execute_batch("INSERT INTO src (id, v) VALUES (1,1),(2,2),(3,3),(4,4),(5,5);")
        .expect("fixture rows");
    connection.execute_batch("INSERT INTO t (id, n) VALUES (1, 5);").expect("trigger fires");

    let mut prepared =
        connection.prepare("SELECT total FROM log ORDER BY total").expect("probe prepares");
    prepared
        .query_map([], |row| row.get::<_, i64>(0))
        .expect("probe runs")
        .collect::<Result<Vec<_>, _>>()
        .expect("probe rows decode")
}

#[test]
fn variable_in_a_projection_item() {
    assert_eq!(totals_after_firing("INSERT INTO log (total) SELECT v_n;"), vec![3]);
}

#[test]
fn variable_in_a_where_predicate() {
    assert_eq!(
        totals_after_firing("INSERT INTO log (total) SELECT v FROM src WHERE v > v_n;"),
        vec![4, 5]
    );
}

#[test]
fn variable_in_a_having_predicate() {
    assert_eq!(
        totals_after_firing("INSERT INTO log (total) SELECT sum(v) FROM src HAVING sum(v) > v_n;"),
        vec![15]
    );
}

#[test]
fn variable_in_a_group_by_expression() {
    assert_eq!(
        totals_after_firing("INSERT INTO log (total) SELECT sum(v) FROM src GROUP BY v - v_n;"),
        vec![1, 2, 3, 4, 5]
    );
}

#[test]
fn variable_in_an_order_by_expression() {
    assert_eq!(
        totals_after_firing(
            "INSERT INTO log (total) SELECT v FROM src ORDER BY abs(v - v_n) LIMIT 1;"
        ),
        vec![3]
    );
}

#[test]
fn variable_in_a_limit_clause() {
    assert_eq!(
        totals_after_firing("INSERT INTO log (total) SELECT v FROM src ORDER BY v LIMIT v_n;"),
        vec![1, 2, 3]
    );
}

#[test]
fn variable_inside_a_derived_table() {
    assert_eq!(
        totals_after_firing("INSERT INTO log (total) SELECT d.c FROM (SELECT v_n * 2 AS c) d;"),
        vec![6]
    );
}

#[test]
fn variable_in_a_join_constraint() {
    assert_eq!(
        totals_after_firing(
            "INSERT INTO log (total) SELECT s.v FROM src s JOIN src u ON u.v = s.v + v_n;"
        ),
        vec![1, 2]
    );
}

#[test]
fn variable_inside_a_correlated_free_subquery() {
    assert_eq!(
        totals_after_firing(
            "INSERT INTO log (total) SELECT v FROM src WHERE EXISTS (SELECT 1 FROM src x WHERE \
             x.v = v_n);"
        ),
        vec![1, 2, 3, 4, 5]
    );
}

#[test]
fn variable_in_a_window_partition() {
    assert_eq!(
        totals_after_firing(
            "INSERT INTO log (total) SELECT DISTINCT count(*) OVER (PARTITION BY v > v_n) FROM src;"
        ),
        vec![2, 3]
    );
}

#[test]
fn variable_in_an_update_assignment() {
    assert_eq!(
        totals_after_firing(
            "INSERT INTO log (total) VALUES (0); UPDATE log SET total = v_n WHERE total = 0;"
        ),
        vec![3]
    );
}

#[test]
fn variable_in_a_delete_predicate() {
    assert_eq!(
        totals_after_firing(
            "INSERT INTO log (total) VALUES (3); INSERT INTO log (total) VALUES (9); DELETE FROM \
             log WHERE total = v_n;"
        ),
        vec![9]
    );
}

#[test]
fn variable_read_twice_sees_one_value() {
    assert_eq!(
        totals_after_firing(
            "INSERT INTO log (total) SELECT v FROM src WHERE v > v_n AND v < v_n \
                             + 2;"
        ),
        vec![4]
    );
}

/// A variable is assigned once, so every row an `UPDATE` touches sees the
/// same value even when the expression is non-deterministic.
///
/// This is why a read is emitted as a scalar subquery rather than the bare
/// expression: SQLite evaluates a subquery that reads nothing from the
/// enclosing row once per statement, and the bare expression once per row.
#[test]
fn a_non_deterministic_variable_is_evaluated_once() {
    let totals = totals_after_declaring(
        "v_r BIGINT := floor(random() * 1000000)::BIGINT;",
        "INSERT INTO log (total) VALUES (0); INSERT INTO log (total) VALUES (0); UPDATE log SET \
         total = v_r;",
    );
    assert_eq!(totals.len(), 2, "both rows should survive: {totals:?}");
    assert_eq!(totals[0], totals[1], "one assignment, so one value: {totals:?}");
}
