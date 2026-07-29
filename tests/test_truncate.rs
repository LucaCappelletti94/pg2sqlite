//! `TRUNCATE` must become `DELETE FROM`, not vanish.
//!
//! `TRUNCATE` is a data-destroying statement, and it used to sit in the
//! `unsupported_statement_patterns!()` catch-all, so a migration that emptied a
//! table produced no output at all: no error, no warning, and a database still
//! holding every row the author meant to remove. `DELETE FROM t` is the exact
//! SQLite equivalent.
//!
//! The options attached to `TRUNCATE` divide cleanly. `CASCADE` changes which
//! rows disappear and has no SQLite form, so it is rejected. `ONLY` and the
//! trailing asterisk concern table inheritance, which `CREATE TABLE ...
//! INHERITS` already rejects outright, so no descendants can exist and both are
//! vacuous.
//!
//! The identity options need care, and the naive reading of them is wrong. This
//! crate emits `AUTOINCREMENT` only for the RLS audit table, never for a user
//! table, so a translated table's primary key is a plain rowid alias with no
//! stored counter. Verified directly: after deleting every row, a rowid alias
//! restarts at 1 while an `AUTOINCREMENT` column continues. So SQLite's natural
//! behaviour is `RESTART IDENTITY`, and it is `CONTINUE IDENTITY` that cannot
//! be honoured, which is the opposite of what one would guess.
//!
//! Row level security is refused outright. PostgreSQL's policies do not apply
//! to `TRUNCATE`, but a translated RLS table is a view whose `INSTEAD OF
//! DELETE` trigger carries the policy predicate, so deleting through it would
//! empty only part of the table.

mod helpers;

use diesel::{prelude::*, sql_query};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
    warnings::TranslationWarning,
};

mod schema {
    diesel::table! {
        docs (id) {
            id -> Integer,
            title -> Text,
        }
    }
    diesel::table! {
        notes (id) {
            id -> Integer,
            body -> Text,
        }
    }
}

use schema::{docs, notes};

const BASE: &str = "
    CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT NOT NULL);
    CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);
";

/// Translates `pg` and applies every emitted statement to a fresh database. The
/// emitted SQL is the artifact under test, so it is applied as generated text.
fn apply(pg: &str) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let statements = Pg2Sqlite::default().sql(pg)?.translate(&Pg2SqliteOptions::default())?;
    let mut conn = SqliteConnection::establish(":memory:")?;
    for statement in &statements {
        sql_query(statement.to_string()).execute(&mut conn)?;
    }
    Ok(conn)
}

/// Seeds both tables through the typed DSL.
fn seed(conn: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    diesel::insert_into(docs::table)
        .values(vec![
            (docs::id.eq(1), docs::title.eq("first")),
            (docs::id.eq(2), docs::title.eq("second")),
        ])
        .execute(conn)?;
    diesel::insert_into(notes::table)
        .values((notes::id.eq(1), notes::body.eq("note")))
        .execute(conn)?;
    Ok(())
}

/// The plan's acceptance criterion: rows go in, a translated TRUNCATE is
/// applied, the table is empty.
#[test]
fn truncate_empties_the_table() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(BASE)?;
    seed(&mut conn)?;

    let statements = Pg2Sqlite::default()
        .sql(&format!("{BASE} TRUNCATE docs;"))?
        .translate(&Pg2SqliteOptions::default())?;
    let truncation = statements.last().expect("a statement must be emitted for TRUNCATE");
    sql_query(truncation.to_string()).execute(&mut conn)?;

    assert_eq!(
        docs::table.count().get_result::<i64>(&mut conn)?,
        0,
        "TRUNCATE must empty the table"
    );
    assert_eq!(
        notes::table.count().get_result::<i64>(&mut conn)?,
        1,
        "an untargeted table must keep its rows"
    );

    Ok(())
}

/// `TRUNCATE` takes a list, and each name becomes its own `DELETE FROM`.
#[test]
fn truncate_empties_every_named_table() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(BASE)?;
    seed(&mut conn)?;

    let statements = Pg2Sqlite::default()
        .sql(&format!("{BASE} TRUNCATE docs, notes;"))?
        .translate(&Pg2SqliteOptions::default())?;
    for statement in statements.iter().skip(2) {
        sql_query(statement.to_string()).execute(&mut conn)?;
    }

    assert_eq!(docs::table.count().get_result::<i64>(&mut conn)?, 0);
    assert_eq!(notes::table.count().get_result::<i64>(&mut conn)?, 0);

    Ok(())
}

/// `TRUNCATE TABLE t` and `TRUNCATE ONLY t` are accepted. `ONLY` restricts the
/// statement to the named table rather than its descendants, and since
/// `CREATE TABLE ... INHERITS` is rejected outright no descendants can exist,
/// so the option asks for the only behaviour SQLite has.
#[test]
fn truncate_accepts_the_table_keyword_and_only() -> Result<(), Box<dyn std::error::Error>> {
    for form in ["TRUNCATE TABLE docs;", "TRUNCATE ONLY docs;", "TRUNCATE TABLE ONLY docs;"] {
        let mut conn = apply(BASE)?;
        seed(&mut conn)?;

        let statements = Pg2Sqlite::default()
            .sql(&format!("{BASE} {form}"))?
            .translate(&Pg2SqliteOptions::default())?;
        let truncation = statements.last().expect("a statement must be emitted");
        sql_query(truncation.to_string()).execute(&mut conn)?;

        assert_eq!(
            docs::table.count().get_result::<i64>(&mut conn)?,
            0,
            "{form} must empty the table"
        );
    }

    Ok(())
}

/// `RESTART IDENTITY` is what SQLite does anyway for a rowid alias, verified in
/// this file's header, so it translates and the identifiers restart.
#[test]
fn truncate_restart_identity_restarts_the_identifiers() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(BASE)?;
    diesel::insert_into(docs::table)
        .values(vec![(docs::title.eq("first")), (docs::title.eq("second"))])
        .execute(&mut conn)?;

    let statements = Pg2Sqlite::default()
        .sql(&format!("{BASE} TRUNCATE docs RESTART IDENTITY;"))?
        .translate(&Pg2SqliteOptions::default())?;
    sql_query(statements.last().expect("a statement must be emitted").to_string())
        .execute(&mut conn)?;

    diesel::insert_into(docs::table).values(docs::title.eq("third")).execute(&mut conn)?;
    assert_eq!(
        docs::table.select(docs::id).first::<i32>(&mut conn)?,
        1,
        "a rowid alias restarts after every row is deleted"
    );

    Ok(())
}

/// `CASCADE` truncates every table holding a foreign key reference to the
/// target. Dropping the keyword would leave those rows in place, so it changes
/// which rows disappear and must be an error rather than a silent loss.
#[test]
fn truncate_cascade_is_rejected() {
    let Err(error) = apply(&format!("{BASE} TRUNCATE docs CASCADE;")) else {
        panic!("TRUNCATE CASCADE must not be translated as though it were a plain TRUNCATE");
    };
    let error = error.to_string();
    assert!(error.contains("CASCADE"), "the error must name the option, got: {error}");
}

/// `CONTINUE IDENTITY` asks to preserve a counter the emitted schema does not
/// keep, since no user table gets `AUTOINCREMENT`. The rows are still deleted,
/// so this warns rather than failing.
#[test]
fn truncate_continue_identity_warns() -> Result<(), Box<dyn std::error::Error>> {
    let report = Pg2Sqlite::default()
        .sql(&format!("{BASE} TRUNCATE docs CONTINUE IDENTITY;"))?
        .translate_with_report(&Pg2SqliteOptions::default())?;

    assert!(
        report.warnings.iter().any(|warning| matches!(
            warning,
            TranslationWarning::LossyDrop { construct, .. } if construct.contains("CONTINUE IDENTITY")
        )),
        "expected a lossy-drop warning for CONTINUE IDENTITY, got {:?}",
        report.warnings
    );

    Ok(())
}

/// A `TRUNCATE` against an RLS table must be refused rather than quietly
/// becoming a partial delete.
///
/// This is the one case where translating `TRUNCATE` at all is unsafe. The
/// emitted `DELETE FROM docs` names the RLS view, and the view's
/// `INSTEAD OF DELETE` trigger carries the policy predicate, so the statement
/// would remove only the rows the policy admits and report success. PostgreSQL
/// ignores policies for `TRUNCATE` and empties the table, so the translated
/// database would silently disagree about which rows survive.
#[test]
fn truncate_of_an_rls_table_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let sql = "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY p ON docs FOR ALL USING (owner_id = 1);
        TRUNCATE docs;
    ";

    let Err(error) = Pg2Sqlite::default().sql(sql)?.translate(&options) else {
        panic!("TRUNCATE of an RLS table must not become a policy-filtered partial delete");
    };
    let error = error.to_string();
    assert!(
        error.contains("row level security"),
        "the error must explain the RLS conflict, got: {error}"
    );

    Ok(())
}
