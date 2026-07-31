//! `EXCEPTION` blocks in a PL/pgSQL body.
//!
//! SQLite triggers have no exception handling, so there is nothing to emit. The
//! deliverable is the error: it has to name exception handling rather than the
//! statement that happens to follow the keyword, which is what the SQL parser
//! reports when the block reaches it unrecognised.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn refuse(body: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!(
            "CREATE TABLE t (id INT PRIMARY KEY, val INT);
             CREATE TABLE log (note TEXT);
             CREATE FUNCTION f() RETURNS trigger AS $$
             {body}
             $$ LANGUAGE plpgsql;
             CREATE TRIGGER go BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();"
        ))
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("SQLite has no exception handling")
        .to_string()
}

#[test]
fn an_exception_block_names_exception_handling() {
    let error = refuse(
        "BEGIN
             INSERT INTO log (note) VALUES ('before');
         EXCEPTION
             WHEN unique_violation THEN
                 INSERT INTO log (note) VALUES ('caught');
         END;",
    );
    // The body is echoed in a parser error, so merely containing the word
    // proves nothing. The message has to be the translator's own.
    assert!(
        !error.contains("sql parser error"),
        "the parser error names the statement after the keyword, not the cause: {error}"
    );
    assert!(
        error.to_lowercase().contains("exception handling"),
        "the error must name exception handling, got: {error}"
    );
}

/// A handler is `EXCEPTION WHEN`, so none of these is one: `RAISE EXCEPTION`
/// shares the keyword and is the common case by far.
#[test]
fn the_word_exception_elsewhere_is_not_a_handler() {
    for body in [
        "BEGIN
             IF NEW.val < 0 THEN
                 RAISE EXCEPTION 'negative';
             END IF;
             RETURN NEW;
         END;",
        "DECLARE
             v_msg TEXT := 'EXCEPTION in a literal';
         BEGIN
             INSERT INTO log (note) VALUES (v_msg);
             RETURN NEW;
         END;",
        "BEGIN
             -- EXCEPTION in a comment
             INSERT INTO log (note) VALUES ('ok');
             RETURN NEW;
         END;",
    ] {
        Pg2Sqlite::default()
            .sql(&format!(
                "CREATE TABLE t (id INT PRIMARY KEY, val INT);
                 CREATE TABLE log (note TEXT);
                 CREATE FUNCTION f() RETURNS trigger AS $$
                 {body}
                 $$ LANGUAGE plpgsql;
                 CREATE TRIGGER go BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();"
            ))
            .expect("parse")
            .translate(&Pg2SqliteOptions::default())
            .unwrap_or_else(|error| panic!("this is not a handler: {error}\n{body}"));
    }
}
