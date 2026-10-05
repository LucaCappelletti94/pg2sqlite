//! A guarded `INSERT ... VALUES` inside a trigger's `IF`.
//!
//! The IF becomes a condition appended to each statement's WHERE clause, and an
//! `INSERT ... VALUES` has none, so the guard had nowhere to go and the insert
//! ran for every row. Rewriting it as `INSERT ... SELECT <values> WHERE
//! <guard>` gives the guard a place and is the ordinary conditional-insert
//! idiom.
//!
//! Measured on PostgreSQL 16: a trigger logging only when `NEW.val > 10`, over
//! inserts of 5 and 50, logs one row.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
     CREATE TABLE log (note TEXT);
     CREATE FUNCTION note_it() RETURNS trigger AS $$
     BEGIN
         IF NEW.val > 10 THEN
             INSERT INTO log (note) VALUES ('big');
         END IF;
         RETURN NEW;
     END;
     $$ LANGUAGE plpgsql;
     CREATE TRIGGER n AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION note_it();";

#[test]
fn a_guarded_insert_runs_only_when_the_condition_holds() {
    let statements = Pg2Sqlite::default()
        .sql(SCHEMA)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate");

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }

    for (id, val) in [(1, 5), (2, 50)] {
        connection.execute("INSERT INTO t VALUES (?1, ?2)", [id, val]).expect("insert");
    }

    let logged: i64 =
        connection.query_row("SELECT count(*) FROM log", [], |row| row.get(0)).expect("count");
    assert_eq!(logged, 1, "only the row over the threshold may be logged");
}

/// The rewrite carries one row, so a multi-row VALUES is refused rather than
/// losing every row but the first. This is the boundary the guard rewrite has,
/// and nothing pinned it before.
#[test]
fn a_multi_row_guarded_insert_is_refused() {
    let error = Pg2Sqlite::default()
        .sql(&SCHEMA.replace("VALUES ('big');", "VALUES ('big'), ('bigger');"))
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("only one row can be carried through the rewrite")
        .to_string();
    assert!(error.contains("Multi-row"), "the error must name the shape, got: {error}");
}
