//! Hazards for the PL/pgSQL preprocessor's text scanning.
//!
//! Every case here puts something that looks like PL/pgSQL syntax where the
//! scanner must not react to it: inside a string literal, inside a comment, or
//! inside a longer identifier. Each must translate correctly or fail with a
//! clear error, never silently corrupt the body.
//!
//! The literal cases read the declared value back out of the row, so a default
//! that is mangled rather than lost is visible.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

/// Translate a trigger whose function body is `body`, then apply it.
fn apply(body: &str) -> Result<Connection, String> {
    let statements = Pg2Sqlite::default()
        .sql(&format!(
            "CREATE TABLE t (id INT PRIMARY KEY, val INT, note TEXT);
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
    Ok(connection)
}

/// Declare `v_msg` with `default`, write it into the row, and return what was
/// stored. A default the scanner cut short comes back short.
fn stored_default(default: &str) -> Result<Option<String>, String> {
    let connection = apply(&format!(
        "DECLARE
             v_msg TEXT := {default};
         BEGIN
             INSERT INTO log (note) VALUES (v_msg);
             RETURN NEW;
         END;"
    ))?;
    connection.execute("INSERT INTO t VALUES (1, 5, NULL)", []).map_err(|e| e.to_string())?;
    connection
        .query_row("SELECT note FROM log", [], |row| row.get(0))
        .map_err(|error| error.to_string())
}

/// R52: `BEGIN` and `END` inside a DECLARE default split the body in the wrong
/// place.
#[test]
fn a_block_keyword_inside_a_literal_is_not_a_block() {
    assert_eq!(
        stored_default("'BEGIN nothing here END'").expect("must translate"),
        Some("BEGIN nothing here END".to_string())
    );
}

/// R53: a semicolon inside a DECLARE default ends the declaration early.
#[test]
fn a_semicolon_inside_a_literal_does_not_end_the_declaration() {
    assert_eq!(
        stored_default("'SELECT 1; SELECT 2'").expect("must translate"),
        Some("SELECT 1; SELECT 2".to_string())
    );
}

/// R54: `RAISE` inside a literal is rewritten as if it were a statement.
#[test]
fn a_raise_inside_a_literal_is_not_a_raise() {
    assert_eq!(
        stored_default("'RAISE EXCEPTION error text'").expect("must translate"),
        Some("RAISE EXCEPTION error text".to_string())
    );
}

/// R56: the SQL `''` escape ends the string early for three of the four
/// scanners, so the rest of the body is read as unquoted text.
#[test]
fn a_doubled_quote_stays_inside_the_string() {
    assert_eq!(
        stored_default("'it''s done; RAISE EXCEPTION here'").expect("must translate"),
        Some("it's done; RAISE EXCEPTION here".to_string())
    );
}

/// R57: a dollar-quoted default with its own tag.
#[test]
fn a_dollar_quoted_default_is_scanned_as_one_token() {
    assert_eq!(
        stored_default("$tag$BEGIN; RAISE EXCEPTION inside$tag$").expect("must translate"),
        Some("BEGIN; RAISE EXCEPTION inside".to_string())
    );
}

/// A keyword inside a comment is not a keyword.
#[test]
fn a_keyword_inside_a_comment_is_not_a_keyword() {
    for comment in ["-- RAISE EXCEPTION never happens; END", "/* RAISE EXCEPTION happens; END */"] {
        let connection = apply(&format!(
            "BEGIN
                 {comment}
                 INSERT INTO log (note) VALUES ('ok');
                 RETURN NEW;
             END;"
        ))
        .unwrap_or_else(|error| panic!("`{comment}` must be ignored: {error}"));
        connection.execute("INSERT INTO t VALUES (1, 5, NULL)", []).expect("insert");
        let note: Option<String> =
            connection.query_row("SELECT note FROM log", [], |row| row.get(0)).expect("row");
        assert_eq!(note, Some("ok".to_string()), "{comment}");
    }
}

/// R55: an identifier that merely contains a keyword must not be rewritten.
#[test]
fn an_identifier_containing_a_keyword_is_left_alone() {
    let connection = apply(
        "DECLARE
             myelsif INT := 1;
         BEGIN
             IF NEW.val > 0 THEN
                 INSERT INTO log (note) VALUES ('positive');
             END IF;
             RETURN NEW;
         END;",
    )
    .expect("the identifier must not be rewritten");
    connection.execute("INSERT INTO t VALUES (1, 5, NULL)", []).expect("insert");
    let note: Option<String> =
        connection.query_row("SELECT note FROM log", [], |row| row.get(0)).expect("row");
    assert_eq!(note, Some("positive".to_string()));
}
