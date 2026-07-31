//! Assignments whose right-hand side spans several lines.
//!
//! The transform read one line at a time, so `v := ` on its own line found an
//! empty expression, gave up, and left the raw `:=` for the SQL parser, which
//! reported `Expected: an SQL statement, found: v`. A statement ends at an
//! unquoted semicolon, not at a newline.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

/// Run `body` as a BEFORE INSERT trigger and return what it logged.
fn logged(body: &str) -> Result<Option<String>, String> {
    let statements = Pg2Sqlite::default()
        .sql(&format!(
            "CREATE TABLE t (id INT PRIMARY KEY, val INT);
             CREATE TABLE log (note TEXT);
             CREATE FUNCTION f() RETURNS trigger AS $$
             {body}
             $$ LANGUAGE plpgsql;
             CREATE TRIGGER go BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();"
        ))
        .map_err(|error| error.to_string())?
        .translate_to_sql(&Pg2SqliteOptions::default())
        .map_err(|error| error.to_string())?;

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection
            .execute_batch(&format!("{statement};"))
            .map_err(|error| format!("{error}\n{statement}"))?;
    }
    connection.execute("INSERT INTO t VALUES (1, 5)", []).map_err(|e| e.to_string())?;
    connection
        .query_row("SELECT note FROM log", [], |row| row.get(0))
        .map_err(|error| error.to_string())
}

/// A CASE spread over several lines, which is the shape the item names.
#[test]
fn a_multi_line_case_assignment_translates() {
    assert_eq!(
        logged(
            "DECLARE
                 v_label TEXT;
             BEGIN
                 v_label :=
                     CASE
                         WHEN NEW.val > 3 THEN 'big'
                         ELSE 'small'
                     END;
                 INSERT INTO log (note) VALUES (v_label);
                 RETURN NEW;
             END;"
        )
        .expect("a multi-line assignment must translate"),
        Some("big".to_string())
    );
}

/// A COALESCE chain broken across lines, the other common shape.
#[test]
fn a_multi_line_coalesce_assignment_translates() {
    assert_eq!(
        logged(
            "DECLARE
                 v_label TEXT;
             BEGIN
                 v_label := COALESCE(
                     NULL,
                     'fallback'
                 );
                 INSERT INTO log (note) VALUES (v_label);
                 RETURN NEW;
             END;"
        )
        .expect("a multi-line assignment must translate"),
        Some("fallback".to_string())
    );
}

/// A single-line assignment is the same statement shape and must keep working.
#[test]
fn a_single_line_assignment_still_translates() {
    assert_eq!(
        logged(
            "DECLARE
                 v_label TEXT;
             BEGIN
                 v_label := 'plain';
                 INSERT INTO log (note) VALUES (v_label);
                 RETURN NEW;
             END;"
        )
        .expect("a single-line assignment must translate"),
        Some("plain".to_string())
    );
}

/// A semicolon inside the right-hand side does not end the statement, which is
/// what makes the split scanner-based rather than a plain `split(';')`.
#[test]
fn a_semicolon_inside_the_expression_does_not_end_it() {
    assert_eq!(
        logged(
            "DECLARE
                 v_label TEXT;
             BEGIN
                 v_label :=
                     'a; b';
                 INSERT INTO log (note) VALUES (v_label);
                 RETURN NEW;
             END;"
        )
        .expect("the literal must not split the statement"),
        Some("a; b".to_string())
    );
}
