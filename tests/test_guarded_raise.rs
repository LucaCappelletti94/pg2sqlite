//! A `RAISE EXCEPTION` guarded by an `IF` inside a trigger body.
//!
//! The IF becomes a condition appended to each statement's WHERE clause, but a
//! `RAISE` is rewritten to `SELECT RAISE(ABORT, ...)`, which has no WHERE to
//! append to. The injection returned an error for that shape and the error was
//! discarded, so the RAISE ran for every row and the trigger aborted
//! unconditionally.
//!
//! The RLS triggers already emit the right idiom, `SELECT RAISE(ABORT, ...)
//! WHERE NOT (...)`, so the guard has a place to go.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, val INT);
     CREATE FUNCTION reject_negative() RETURNS trigger AS $$
     BEGIN
         IF NEW.val < 0 THEN
             RAISE EXCEPTION 'val must not be negative';
         END IF;
         RETURN NEW;
     END;
     $$ LANGUAGE plpgsql;
     CREATE TRIGGER guard BEFORE INSERT ON t FOR EACH ROW
         EXECUTE FUNCTION reject_negative();";

fn applied() -> Connection {
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
    connection
}

#[test]
fn a_row_the_guard_permits_is_inserted() {
    let connection = applied();
    connection.execute("INSERT INTO t VALUES (1, 5)", []).expect("a positive value is allowed");
    let stored: i64 =
        connection.query_row("SELECT val FROM t WHERE id = 1", [], |row| row.get(0)).expect("row");
    assert_eq!(stored, 5);
}

#[test]
fn a_row_the_guard_rejects_aborts() {
    let connection = applied();
    let error = connection
        .execute("INSERT INTO t VALUES (2, -1)", [])
        .expect_err("a negative value must abort");
    assert!(
        error.to_string().contains("val must not be negative"),
        "the abort must carry the message, got: {error}"
    );
    let remaining: i64 =
        connection.query_row("SELECT count(*) FROM t", [], |row| row.get(0)).expect("count");
    assert_eq!(remaining, 0, "the rejected row must not be stored");
}
