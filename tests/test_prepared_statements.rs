//! `PREPARE`, `EXECUTE`, and `DEALLOCATE` must be refused, not discarded.
//!
//! All three used to sit in the `unsupported_statement_patterns!()` catch-all
//! and vanish. `EXECUTE` is the serious case: it performs the actual work of
//! the prepared statement, so dropping it loses whatever the migration meant to
//! do, silently.
//!
//! PostgreSQL's server-side prepared statements are SQL statements. SQLite has
//! no equivalent, because preparing there is a C API operation
//! (`sqlite3_prepare_v2`) with no statement form and no server-side name to
//! refer to later. There is nothing to emit.
//!
//! `DEALLOCATE` is refused too, and it is the one case worth justifying, since
//! its own effect is only to free a named plan and so is result-neutral in
//! isolation. It is still bucket 1 under D2: a script that deallocates must
//! have prepared first, that `PREPARE` is an error, and a `DEALLOCATE` naming a
//! statement which cannot exist in the output is not something to accept
//! quietly.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

const BASE: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER);";

/// Translates `pg` and returns the error message, failing if translation
/// succeeds.
fn rejection(pg: &str) -> String {
    let result = Pg2Sqlite::default()
        .sql(pg)
        .expect("the fixture must parse")
        .translate(&Pg2SqliteOptions::default());

    match result {
        Ok(statements) => {
            panic!("the statement must be refused rather than dropped, got {statements:?}")
        }
        Err(error) => error.to_string(),
    }
}

/// `PREPARE` carries a statement definition that has nowhere to go.
#[test]
fn prepare_is_rejected() {
    let error = rejection(&format!("{BASE} PREPARE p (INT) AS SELECT n FROM t WHERE id = $1;"));
    assert!(error.contains("PREPARE"), "the error must name the statement, got: {error}");
}

/// `EXECUTE` is the data-loss case: it runs the work.
#[test]
fn execute_is_rejected() {
    let error = rejection(&format!("{BASE} EXECUTE p(1);"));
    assert!(error.contains("EXECUTE"), "the error must name the statement, got: {error}");
}

/// `EXECUTE` with no arguments is refused on the same grounds.
#[test]
fn execute_without_parameters_is_rejected() {
    let error = rejection(&format!("{BASE} EXECUTE p;"));
    assert!(error.contains("EXECUTE"), "the error must name the statement, got: {error}");
}

/// `DEALLOCATE` is refused, per the reasoning in this file's header.
#[test]
fn deallocate_is_rejected() {
    let error = rejection(&format!("{BASE} DEALLOCATE p;"));
    assert!(error.contains("DEALLOCATE"), "the error must name the statement, got: {error}");
}

/// `DEALLOCATE ALL` too, which names no single statement.
#[test]
fn deallocate_all_is_rejected() {
    let error = rejection(&format!("{BASE} DEALLOCATE ALL;"));
    assert!(error.contains("DEALLOCATE"), "the error must name the statement, got: {error}");
}

/// `CREATE TRIGGER ... EXECUTE FUNCTION` must keep working.
///
/// It shares the `EXECUTE` keyword but is a trigger clause rather than
/// `Statement::Execute`, so it is a different code path. Guards the rejection
/// above against catching the trigger syntax the crate translates heavily.
#[test]
fn execute_function_in_a_trigger_still_translates() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER);
        CREATE FUNCTION bump() RETURNS TRIGGER AS $$
        BEGIN
            NEW.n := NEW.n + 1;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER t_bump BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION bump();
    ";

    let statements = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    assert!(
        statements.iter().any(|s| s.to_string().contains("CREATE TRIGGER")),
        "the trigger must still be emitted: {statements:?}"
    );
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for stmt in &statements {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("emitted statement must run in SQLite: {e}\n{stmt}"));
    }

    Ok(())
}
