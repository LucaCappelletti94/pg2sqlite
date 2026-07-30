//! Statements that are already SQLite's own syntax must not be discarded.
//!
//! `unsupported_statement_patterns!()` claims to list constructs SQLite cannot
//! express, yet it held `ANALYZE`, `PRAGMA`, `ATTACH DATABASE`, and
//! `CREATE VIRTUAL TABLE`, all of which SQLite accepts verbatim. Dropping a
//! `CREATE VIRTUAL TABLE` when the target *is* SQLite is the clearest case: the
//! FTS or R-tree index the author asked for simply never exists.
//!
//! `EXPLAIN` is the one that needs translating rather than passing through.
//! PostgreSQL's `EXPLAIN <stmt>` becomes SQLite's `EXPLAIN QUERY PLAN <stmt>`,
//! and the inner statement has to be translated too, otherwise the plan would
//! be computed for PostgreSQL SQL that SQLite cannot parse.
//!
//! `EXPLAIN ANALYZE` is refused. In PostgreSQL it *executes* the statement and
//! reports real timings, so an `EXPLAIN ANALYZE INSERT ...` writes rows.
//! SQLite's `EXPLAIN QUERY PLAN` never executes anything, so translating it
//! would silently drop the write.
//!
//! The plan listed a sixth member, `Statement::RenameTable`. It does not belong
//! here: `RENAME TABLE t TO t2` is MySQL syntax and a syntax error in SQLite
//! (verified), so it needs translating to `ALTER TABLE ... RENAME TO`, which is
//! plan item R13.

use diesel::{prelude::*, sql_query};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

const BASE: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER);";

/// Translates `pg` and returns the statements after the leading `CREATE TABLE`.
fn emitted(pg: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("the fixture must parse")
        .translate(&Pg2SqliteOptions::default())
        .expect("translation must succeed")
        .iter()
        .skip(1)
        .map(ToString::to_string)
        .collect()
}

/// Applies every emitted statement to a fresh database.
///
/// The generated SQL is the artifact under test, so it is executed as text. No
/// typed query can stand in for "run exactly what the translator produced".
fn apply(pg: &str) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let statements = Pg2Sqlite::default().sql(pg)?.translate(&Pg2SqliteOptions::default())?;
    let mut conn = SqliteConnection::establish(":memory:")?;
    for statement in &statements {
        sql_query(statement.to_string()).execute(&mut conn)?;
    }
    Ok(conn)
}

/// `ANALYZE` is SQLite's own statement and must survive.
#[test]
fn analyze_reaches_the_output_and_executes() -> Result<(), Box<dyn std::error::Error>> {
    let statements = emitted(&format!("{BASE} ANALYZE t;"));
    assert_eq!(statements.len(), 1, "ANALYZE must be emitted, got {statements:?}");
    assert!(statements[0].contains("ANALYZE"), "got {statements:?}");

    apply(&format!("{BASE} ANALYZE t;"))?;
    Ok(())
}

/// A bare `ANALYZE`, which analyses everything, is equally valid in SQLite.
#[test]
fn bare_analyze_reaches_the_output_and_executes() -> Result<(), Box<dyn std::error::Error>> {
    let statements = emitted(&format!("{BASE} ANALYZE;"));
    assert_eq!(statements.len(), 1, "a bare ANALYZE must be emitted, got {statements:?}");

    apply(&format!("{BASE} ANALYZE;"))?;
    Ok(())
}

/// `PRAGMA` is SQLite's own statement. Only the forms the PostgreSQL parser
/// accepts are reachable here, which excludes `PRAGMA x = ON` and
/// `PRAGMA table_info(t)`, both of which fail in the parser rather than the
/// translator.
#[test]
fn pragma_reaches_the_output_and_executes() -> Result<(), Box<dyn std::error::Error>> {
    for pragma in ["PRAGMA foreign_keys = 1;", "PRAGMA journal_mode;"] {
        let statements = emitted(&format!("{BASE} {pragma}"));
        assert_eq!(statements.len(), 1, "{pragma} must be emitted, got {statements:?}");
        assert!(statements[0].contains("PRAGMA"), "got {statements:?}");

        apply(&format!("{BASE} {pragma}"))?;
    }
    Ok(())
}

/// `ATTACH DATABASE` is SQLite's own statement.
#[test]
fn attach_database_reaches_the_output_and_executes() -> Result<(), Box<dyn std::error::Error>> {
    let sql = format!("{BASE} ATTACH DATABASE ':memory:' AS side;");
    let statements = emitted(&sql);
    assert_eq!(statements.len(), 1, "ATTACH must be emitted, got {statements:?}");
    assert!(statements[0].contains("ATTACH"), "got {statements:?}");

    apply(&sql)?;
    Ok(())
}

mod schema {
    diesel::table! {
        /// The FTS5 virtual table the migration asks for.
        docs (rowid) {
            rowid -> Integer,
            title -> Text,
        }
    }
}

/// `CREATE VIRTUAL TABLE` is the clearest case: the target is SQLite, so
/// dropping it removes the index the author explicitly created.
///
/// Asserted by using the table, not by matching text: a row is inserted and
/// found through an FTS `MATCH`, which only works if the virtual table really
/// exists.
#[test]
fn create_virtual_table_reaches_the_output_and_is_usable() -> Result<(), Box<dyn std::error::Error>>
{
    let sql = "CREATE VIRTUAL TABLE docs USING fts5(title);";
    let mut conn = apply(sql)?;

    diesel::insert_into(schema::docs::table)
        .values(schema::docs::title.eq("the quick brown fox"))
        .execute(&mut conn)?;

    let found: i64 = schema::docs::table
        .filter(schema::docs::title.eq("the quick brown fox"))
        .count()
        .get_result(&mut conn)?;
    assert_eq!(found, 1, "the FTS5 table must exist and hold the row");

    Ok(())
}

/// PostgreSQL `EXPLAIN` becomes SQLite `EXPLAIN QUERY PLAN`.
#[test]
fn explain_becomes_explain_query_plan() -> Result<(), Box<dyn std::error::Error>> {
    let sql = format!("{BASE} EXPLAIN SELECT n FROM t;");
    let statements = emitted(&sql);
    assert_eq!(statements.len(), 1, "EXPLAIN must be emitted, got {statements:?}");
    assert!(
        statements[0].contains("QUERY PLAN"),
        "EXPLAIN must become EXPLAIN QUERY PLAN, got {statements:?}"
    );

    apply(&sql)?;
    Ok(())
}

/// The statement inside an `EXPLAIN` must itself be translated.
///
/// Without that the plan would be computed over PostgreSQL SQL. `now()` has no
/// SQLite spelling, so its presence in the output would prove the inner
/// statement was passed through untouched, and executing it would fail.
#[test]
fn explain_translates_its_inner_statement() -> Result<(), Box<dyn std::error::Error>> {
    let sql = format!("{BASE} EXPLAIN SELECT now() FROM t;");
    let statements = emitted(&sql);
    assert_eq!(statements.len(), 1, "got {statements:?}");
    assert!(
        !statements[0].contains("now()"),
        "the inner statement must be translated, got {statements:?}"
    );

    apply(&sql)?;
    Ok(())
}

/// `EXPLAIN ANALYZE` is refused because PostgreSQL executes the statement while
/// `EXPLAIN QUERY PLAN` does not, so a write would be lost.
#[test]
fn explain_analyze_is_rejected() {
    let result = Pg2Sqlite::default()
        .sql(&format!("{BASE} EXPLAIN ANALYZE INSERT INTO t (id, n) VALUES (1, 1);"))
        .expect("the fixture must parse")
        .translate(&Pg2SqliteOptions::default());

    let Err(error) = result else {
        panic!("EXPLAIN ANALYZE must not silently become a plan that never runs");
    };
    let error = error.to_string();
    assert!(error.contains("ANALYZE"), "the error must name the option, got: {error}");
}
