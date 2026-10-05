//! F9: four PL/pgSQL defects, each reproduced by running the emitted trigger.
//!
//! Every expectation was measured on PostgreSQL 17 before the fix. These tests
//! apply the translated schema, fire the trigger with an INSERT, and read back
//! what the trigger wrote, because all four defects are invisible in the
//! emitted text and only surface when the trigger runs.
//!
//! 9a: a variable used in an `INSERT ... SELECT` source was never substituted.
//! 9b: the `ELSIF` rewrite matched inside an identifier, so a column named
//!     `preelsif` became `preELSEIF`.
//! 9c: the `ELSIF` rewrite could not see a dollar-quoted literal, so it rewrote
//!     inside one, and the literal itself reached SQLite in a syntax it has no
//!     form for.
//! 9d: a variable whose name begins with `SELECT` was misclassified by a
//!     substring test and left unbound.

mod helpers;

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[derive(QueryableByName, Debug, PartialEq, Eq)]
struct Pair {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    a: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    b: Option<String>,
}

/// Applies the translated script, then reads `probe` back.
fn run(pg: &str, probe: &str) -> Vec<Pair> {
    let statements = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate");
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    for statement in &statements {
        diesel::sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted statement failed: {statement}: {error}"));
    }
    diesel::sql_query(probe).load::<Pair>(&mut connection).expect("probe")
}

// ---------------------------------------------------------------------------
// 9a: a variable inside an INSERT ... SELECT source
// ---------------------------------------------------------------------------

/// PostgreSQL writes `('processed', 'trigger_body')`. The emitted trigger
/// referenced the bare variable name and failed with `no such column: v_label`.
#[test]
fn a_variable_reaches_an_insert_select_source() {
    let rows = run(
        "CREATE TABLE events (id INT PRIMARY KEY);
         CREATE TABLE audit (label TEXT, src TEXT);
         CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$
         DECLARE v_label TEXT := 'processed';
         BEGIN
           INSERT INTO audit (label, src) SELECT v_label, 'trigger_body' FROM (SELECT 1) AS dummy;
           RETURN NEW;
         END; $$;
         CREATE TRIGGER t AFTER INSERT ON events FOR EACH ROW EXECUTE FUNCTION f();
         INSERT INTO events VALUES (1);",
        "SELECT label AS a, src AS b FROM audit",
    );
    assert_eq!(rows, vec![Pair { a: Some("processed".into()), b: Some("trigger_body".into()) }]);
}

// ---------------------------------------------------------------------------
// 9b: the ELSIF rewrite inside an identifier
// ---------------------------------------------------------------------------

/// A column whose name merely ends in `elsif` was corrupted into `preELSEIF`,
/// so the emitted trigger could not find it.
#[test]
fn an_identifier_ending_in_the_keyword_survives() {
    let rows = run(
        "CREATE TABLE events (id INT PRIMARY KEY);
         CREATE TABLE data (id INT, preelsif TEXT);
         CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO data (id, preelsif) VALUES (NEW.id, 'ok');
           RETURN NEW;
         END; $$;
         CREATE TRIGGER t AFTER INSERT ON events FOR EACH ROW EXECUTE FUNCTION f();
         INSERT INTO events VALUES (1);",
        "SELECT preelsif AS a, CAST(id AS TEXT) AS b FROM data",
    );
    assert_eq!(rows, vec![Pair { a: Some("ok".into()), b: Some("1".into()) }]);
}

/// The keyword still has to be rewritten where it really is one, so the guard
/// above cannot be a blanket refusal to rewrite.
#[test]
fn a_real_elsif_branch_is_still_rewritten() {
    let rows = run(
        "CREATE TABLE events (id INT PRIMARY KEY);
         CREATE TABLE audit (label TEXT, src TEXT);
         CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.id = 0 THEN
             INSERT INTO audit (label, src) VALUES ('zero', 'x');
           ELSIF NEW.id = 1 THEN
             INSERT INTO audit (label, src) VALUES ('one', 'y');
           END IF;
           RETURN NEW;
         END; $$;
         CREATE TRIGGER t AFTER INSERT ON events FOR EACH ROW EXECUTE FUNCTION f();
         INSERT INTO events VALUES (1);",
        "SELECT label AS a, src AS b FROM audit",
    );
    assert_eq!(rows, vec![Pair { a: Some("one".into()), b: Some("y".into()) }]);
}

// ---------------------------------------------------------------------------
// 9c: dollar-quoted literals
// ---------------------------------------------------------------------------

/// PostgreSQL stores the literal verbatim. The rewrite could not see the
/// dollar quotes, so it changed the text inside them, and the literal itself
/// reached SQLite in a syntax it has no form for.
#[test]
fn a_dollar_quoted_literal_is_stored_verbatim() {
    let rows = run(
        "CREATE TABLE events (id INT PRIMARY KEY);
         CREATE TABLE log (sql_text TEXT, note TEXT);
         CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO log (sql_text, note)
             VALUES ($tag$CASE WHEN x ELSIF y THEN 1 END$tag$, 'kept');
           RETURN NEW;
         END; $$;
         CREATE TRIGGER t AFTER INSERT ON events FOR EACH ROW EXECUTE FUNCTION f();
         INSERT INTO events VALUES (1);",
        "SELECT sql_text AS a, note AS b FROM log",
    );
    assert_eq!(
        rows,
        vec![Pair { a: Some("CASE WHEN x ELSIF y THEN 1 END".into()), b: Some("kept".into()) }],
        "the literal must survive byte for byte"
    );
}

/// A dollar-quoted literal holding a single quote has to be re-emitted with
/// the quote doubled, which is the only escape SQLite has.
///
/// The inner tag differs from the body's on purpose. PostgreSQL ends a
/// dollar-quoted span at the first repeat of its own tag, so `$$` inside a
/// `$$` body closes the body, and nesting requires distinct tags. sqlparser
/// reads it the same way and reports an unterminated literal.
#[test]
fn a_dollar_quoted_literal_holding_a_quote_is_escaped() {
    let rows = run(
        "CREATE TABLE events (id INT PRIMARY KEY);
         CREATE TABLE log (sql_text TEXT, note TEXT);
         CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO log (sql_text, note) VALUES ($q$it's here$q$, 'kept');
           RETURN NEW;
         END; $$;
         CREATE TRIGGER t AFTER INSERT ON events FOR EACH ROW EXECUTE FUNCTION f();
         INSERT INTO events VALUES (1);",
        "SELECT sql_text AS a, note AS b FROM log",
    );
    assert_eq!(rows, vec![Pair { a: Some("it's here".into()), b: Some("kept".into()) }]);
}

// ---------------------------------------------------------------------------
// 9d: the persistent-versus-scoped binding heuristic
// ---------------------------------------------------------------------------

/// PostgreSQL takes the second branch and writes `('b', NULL)`, because
/// `v_result` was never assigned on that path. The substring test read the
/// `SELECT` inside the variable's own name as evidence of a subquery, so the
/// variable was left unbound and the trigger failed with `no such column`.
#[test]
fn a_variable_named_like_a_keyword_is_still_bound() {
    let rows = run(
        "CREATE TABLE events (id INT PRIMARY KEY, amount INT);
         CREATE TABLE audit (label TEXT, src TEXT);
         CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$
         DECLARE
           SELECT_FACTOR FLOAT := 2.0;
           v_result FLOAT;
         BEGIN
           IF NEW.id = 1 THEN
             v_result := (SELECT_FACTOR * NEW.amount);
             INSERT INTO audit (label, src) VALUES ('a', CAST(v_result AS TEXT));
           ELSIF NEW.id = 2 THEN
             INSERT INTO audit (label, src) VALUES ('b', CAST(v_result AS TEXT));
           END IF;
           RETURN NEW;
         END; $$;
         CREATE TRIGGER t AFTER INSERT ON events FOR EACH ROW EXECUTE FUNCTION f();
         INSERT INTO events VALUES (2, 10);",
        "SELECT label AS a, src AS b FROM audit",
    );
    assert_eq!(
        rows,
        vec![Pair { a: Some("b".into()), b: None }],
        "the unassigned variable is NULL on this branch, as in PostgreSQL"
    );
}

/// The other branch of the same trigger, so the fix is not just making the
/// variable disappear.
#[test]
fn the_keyword_named_variable_still_carries_its_value() {
    let rows = run(
        "CREATE TABLE events (id INT PRIMARY KEY, amount INT);
         CREATE TABLE audit (label TEXT, src TEXT);
         CREATE FUNCTION f() RETURNS trigger LANGUAGE plpgsql AS $$
         DECLARE
           SELECT_FACTOR FLOAT := 2.0;
           v_result FLOAT;
         BEGIN
           IF NEW.id = 1 THEN
             v_result := (SELECT_FACTOR * NEW.amount);
             INSERT INTO audit (label, src) VALUES ('a', CAST(v_result AS TEXT));
           END IF;
           RETURN NEW;
         END; $$;
         CREATE TRIGGER t AFTER INSERT ON events FOR EACH ROW EXECUTE FUNCTION f();
         INSERT INTO events VALUES (1, 10);",
        "SELECT label AS a, src AS b FROM audit",
    );
    assert_eq!(rows, vec![Pair { a: Some("a".into()), b: Some("20.0".into()) }]);
}
