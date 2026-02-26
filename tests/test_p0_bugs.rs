//! TDD tests for P0 bug fixes.
//!
//! Each test is written RED-first (verifying the bug exists or the fix
//! is in place) and exercises the translated SQL against a real SQLite
//! database via Diesel to confirm runtime correctness.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection as RusqliteConnection;

// ---------------------------------------------------------------------------
// Bug 1: FOR UPDATE / FOR SHARE must be stripped
// ---------------------------------------------------------------------------

/// `FOR UPDATE` is a PostgreSQL row-locking hint that SQLite does not support.
/// The translator must strip it rather than emitting invalid SQLite SQL.
#[test]
fn for_update_stripped_from_select() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE lock_test (id INTEGER PRIMARY KEY, val INTEGER NOT NULL);
        SELECT * FROM lock_test FOR UPDATE;
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();

    assert!(
        !select_sql.to_uppercase().contains("FOR UPDATE"),
        "FOR UPDATE must be stripped from translated output, got: {select_sql}"
    );

    // Confirm the translated DDL + DML actually run in SQLite without error.
    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(())
}

/// `FOR SHARE` is also a PostgreSQL row-locking hint; same treatment required.
#[test]
fn for_share_stripped_from_select() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE lock_test2 (id INTEGER PRIMARY KEY, val INTEGER NOT NULL);
        SELECT * FROM lock_test2 FOR SHARE;
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();

    assert!(
        !select_sql.to_uppercase().contains("FOR SHARE"),
        "FOR SHARE must be stripped from translated output, got: {select_sql}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bug 2: NULLS FIRST / NULLS LAST must be stripped from ORDER BY
// ---------------------------------------------------------------------------

/// `NULLS FIRST` is not valid syntax in SQLite < 3.30 and should be stripped.
/// The translated query must still execute and return rows in the correct
/// order.
#[test]
fn nulls_first_stripped_from_order_by() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE nulls_test (id INTEGER PRIMARY KEY, val INTEGER);
        SELECT val FROM nulls_test ORDER BY val ASC NULLS FIRST;
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();

    let upper = select_sql.to_uppercase();
    assert!(
        !upper.contains("NULLS FIRST") && !upper.contains("NULLS LAST"),
        "NULLS FIRST must be stripped from ORDER BY, got: {select_sql}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    diesel::sql_query("INSERT INTO nulls_test VALUES (1, 10), (2, 5)").execute(&mut conn)?;

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
        val: Option<i32>,
    }
    let rows = diesel::sql_query(select_sql).load::<Row>(&mut conn)?;
    assert_eq!(rows[0].val, Some(5), "Expected ascending sort; first row should be 5");
    Ok(())
}

/// `NULLS LAST` must also be stripped.
#[test]
fn nulls_last_stripped_from_order_by() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE nulls_test2 (id INTEGER PRIMARY KEY, val INTEGER);
        SELECT val FROM nulls_test2 ORDER BY val DESC NULLS LAST;
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();

    let upper = select_sql.to_uppercase();
    assert!(
        !upper.contains("NULLS FIRST") && !upper.contains("NULLS LAST"),
        "NULLS LAST must be stripped from ORDER BY, got: {select_sql}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bug 3: bool_and / bool_or / every must not silently map to MIN / MAX
// ---------------------------------------------------------------------------

/// `bool_and` over booleans has NULL semantics that differ from `MIN`.
/// The translator must refuse to translate it silently and return a clear
/// error.
#[test]
fn bool_and_causes_unsupported_error() {
    let sql = "CREATE TABLE ba_test (id INT PRIMARY KEY, flag BOOLEAN);
               SELECT bool_and(flag) FROM ba_test;";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "bool_and must cause a translation error, not silently map to MIN");
    let err = result.unwrap_err().to_string();
    assert!(
        err.to_lowercase().contains("bool_and"),
        "Error message must mention bool_and, got: {err}"
    );
}

/// `bool_or` must also be rejected.
#[test]
fn bool_or_causes_unsupported_error() {
    let sql = "CREATE TABLE bo_test (id INT PRIMARY KEY, flag BOOLEAN);
               SELECT bool_or(flag) FROM bo_test;";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "bool_or must cause a translation error, not silently map to MAX");
    let err = result.unwrap_err().to_string();
    assert!(
        err.to_lowercase().contains("bool_or"),
        "Error message must mention bool_or, got: {err}"
    );
}

/// `every` is an alias for `bool_and` in PostgreSQL and must also be rejected.
#[test]
fn every_causes_unsupported_error() {
    let sql = "CREATE TABLE ev_test (id INT PRIMARY KEY, flag BOOLEAN);
               SELECT every(flag) FROM ev_test;";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "every must cause a translation error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.to_lowercase().contains("every") || err.to_lowercase().contains("bool"),
        "Error message must mention every or bool, got: {err}"
    );
}

/// `LEAST` must still work — it must NOT be affected by the bool_and fix.
#[test]
fn least_still_translates_correctly() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE le_test (id INT PRIMARY KEY, a INTEGER NOT NULL, b INTEGER NOT NULL);
               SELECT LEAST(a, b) AS result FROM le_test;";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();
    assert!(
        select_sql.to_uppercase().contains("MIN("),
        "LEAST should still become MIN, got: {select_sql}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    diesel::sql_query("INSERT INTO le_test VALUES (1, 3, 7)").execute(&mut conn)?;

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        result: i32,
    }
    let rows = diesel::sql_query(select_sql).load::<Row>(&mut conn)?;
    assert_eq!(rows[0].result, 3, "LEAST(3, 7) should return 3");
    Ok(())
}

// ---------------------------------------------------------------------------
// Bug 4: EXTRACT(WEEK) must not silently use strftime('%W') (Sunday-based)
// ---------------------------------------------------------------------------

/// SQLite's `strftime('%W', ...)` uses Sunday-based week numbers, while
/// PostgreSQL's `EXTRACT(WEEK)` uses ISO 8601 Monday-based week numbers.
/// The translator must refuse to emit `%W` for WEEK extraction.
#[test]
fn extract_week_does_not_emit_percent_w() {
    let sql = "
        CREATE TABLE week_test (id INTEGER PRIMARY KEY, ts TEXT);
        SELECT EXTRACT(WEEK FROM ts) FROM week_test;
    ";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());

    match result {
        Err(e) => {
            // An error is the correct and preferred outcome.
            let msg = e.to_string().to_lowercase();
            assert!(msg.contains("week"), "Error must mention WEEK, got: {e}");
        }
        Ok(stmts) => {
            // If it succeeds it must NOT use %W (Sunday-based).
            let out = stmts
                .iter()
                .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
                .map(|s| s.to_string())
                .unwrap_or_default();
            assert!(
                !out.contains("'%W'"),
                "Must not emit strftime('%W') — that is Sunday-based, not ISO 8601: {out}"
            );
        }
    }
}

/// Verify that the wrong result would have occurred with the old `%W` approach.
/// On 2023-01-01 (Sunday), PG returns week 52 (ISO) but SQLite %W returns 00.
#[test]
fn extract_week_old_percent_w_gives_wrong_result() -> Result<(), Box<dyn std::error::Error>> {
    // This test demonstrates the semantic difference directly via rusqlite,
    // confirming WHY %W is wrong and an error is the right translation.
    let conn = RusqliteConnection::open_in_memory()?;
    let week_w: i64 =
        conn.query_row("SELECT CAST(strftime('%W', '2023-01-01') AS INTEGER)", [], |r| r.get(0))?;
    // strftime('%W', '2023-01-01') = 0 (Sunday-based: week hasn't started yet)
    // PostgreSQL EXTRACT(WEEK FROM '2023-01-01') = 52 (ISO: belongs to week 52 of
    // 2022) They differ — %W is the wrong mapping.
    assert_ne!(
        week_w, 52,
        "strftime('%W') gives {week_w}, not 52 — confirming it is not ISO week numbering"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Bug 5: RAISE INFO 'msg' (single space) must be dropped, not passed through
// ---------------------------------------------------------------------------

/// A PL/pgSQL function with `RAISE INFO 'msg'` (one space after INFO) was
/// previously not detected and passed through as invalid SQLite syntax.
#[test]
fn raise_info_single_space_is_dropped() -> Result<(), Box<dyn std::error::Error>> {
    // The trigger also copies NEW.val into a log table so the body is non-empty
    // after RAISE INFO is stripped (SQLite requires at least one DML statement).
    let sql = r"
        CREATE TABLE ri_test (id INTEGER PRIMARY KEY, val INTEGER NOT NULL);
        CREATE TABLE ri_log (id INTEGER PRIMARY KEY, logged_val INTEGER NOT NULL);
        CREATE OR REPLACE FUNCTION check_ri_val() RETURNS TRIGGER AS $body$
        BEGIN
            RAISE INFO 'checking value';
            INSERT INTO ri_log (id, logged_val) VALUES (NEW.id, NEW.val);
            RETURN NEW;
        END;
        $body$ LANGUAGE plpgsql;
        CREATE TRIGGER check_ri_val_trigger
        AFTER INSERT ON ri_test
        FOR EACH ROW EXECUTE FUNCTION check_ri_val();
    ";

    // Translation must succeed — RAISE INFO is not valid SQLite and must be
    // dropped.
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let out = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");
    assert!(
        !out.contains("RAISE INFO"),
        "RAISE INFO must be dropped from the translated trigger body, got:\n{out}"
    );

    // The resulting DDL must execute without error in SQLite.
    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // The trigger must fire on INSERT into ri_test, populating ri_log.
    diesel::sql_query("INSERT INTO ri_test VALUES (1, 42)").execute(&mut conn)?;

    #[derive(diesel::QueryableByName)]
    struct LogRow {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        logged_val: i32,
    }
    let rows = diesel::sql_query("SELECT logged_val FROM ri_log").load::<LogRow>(&mut conn)?;
    assert_eq!(rows.len(), 1, "Trigger must have fired and inserted one log row");
    assert_eq!(rows[0].logged_val, 42, "logged_val must match inserted val");
    Ok(())
}

// ---------------------------------------------------------------------------
// Bug 6: FTS5 with non-INTEGER primary key must cause a clear error
// ---------------------------------------------------------------------------

/// FTS5 `content_rowid=` maps to the SQLite rowid, which must be an INTEGER.
/// A VARCHAR primary key cannot be used as a rowid.
#[test]
fn fts5_with_varchar_pk_causes_error() {
    let sql = "
        CREATE TABLE fts_varchar_test (id VARCHAR(36) PRIMARY KEY, body TEXT);
        CREATE INDEX fts_varchar_test_fts ON fts_varchar_test USING GIN (to_tsvector('english', body));
    ";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "FTS5 with VARCHAR primary key must cause a translation error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.to_lowercase().contains("integer")
            || err.to_lowercase().contains("rowid")
            || err.to_lowercase().contains("primary key"),
        "Error must explain the INTEGER rowid requirement, got: {err}"
    );
}

/// TEXT primary key is also not a valid rowid.
#[test]
fn fts5_with_text_pk_causes_error() {
    let sql = "
        CREATE TABLE fts_text_pk (id TEXT PRIMARY KEY, body TEXT);
        CREATE INDEX fts_text_pk_idx ON fts_text_pk USING GIN (to_tsvector('english', body));
    ";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "FTS5 with TEXT primary key must cause a translation error");
}

/// INTEGER primary key must still work and produce a runnable FTS5 table.
#[test]
fn fts5_with_integer_pk_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE fts_int_pk (id INTEGER PRIMARY KEY, body TEXT);
        CREATE INDEX fts_int_pk_idx ON fts_int_pk USING GIN (to_tsvector('english', body));
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let mut conn = SqliteConnection::establish(":memory:")?;
    // sqlite-vec is not loaded here, but rusqlite can load fts5 which is built in.
    // We use rusqlite for this test since sqlite-vec is needed for vec0.
    // FTS5 is part of SQLite itself.
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(())
}
