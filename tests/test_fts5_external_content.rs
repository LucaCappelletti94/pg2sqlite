//! TDD tests for FTS5 external content mode translation (Section 5).

#![allow(missing_docs)]

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[test]
fn test_fts5_uses_external_content_mode() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, body TEXT);
        CREATE INDEX docs_fts ON docs USING GIN (to_tsvector('english', body));
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let out = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");

    // Must use external content mode
    assert!(out.contains("content="), "FTS5 must use content= (external content mode), got: {out}");
    assert!(out.contains("content_rowid="), "FTS5 must specify content_rowid=, got: {out}");

    // Execute all translated statements to verify the output is valid SQLite.
    {
        let conn = rusqlite::Connection::open_in_memory()?;
        for stmt in &translated {
            conn.execute_batch(&format!("{stmt};"))?;
        }
    }
    Ok(())
}

#[test]
fn test_fts5_external_content_search_works() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, body TEXT);
        CREATE INDEX docs_fts ON docs USING GIN (to_tsvector('english', body));
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let mut conn = SqliteConnection::establish(":memory:")?;

    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // Insert rows — sync triggers should populate the FTS index
    diesel::sql_query("INSERT INTO docs VALUES (1, 'hello world')").execute(&mut conn)?;
    diesel::sql_query("INSERT INTO docs VALUES (2, 'goodbye world')").execute(&mut conn)?;
    diesel::sql_query("INSERT INTO docs VALUES (3, 'hello SQLite')").execute(&mut conn)?;

    // FTS search for 'hello'
    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
    }

    let rows = diesel::sql_query(
        "SELECT docs.id FROM docs JOIN docs_fts ON docs.rowid = docs_fts.rowid \
         WHERE docs_fts MATCH 'hello'",
    )
    .load::<Row>(&mut conn)?;
    let mut ids: Vec<i32> = rows.into_iter().map(|r| r.id).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 3], "Should match rows containing 'hello'");

    // Delete row 1 — DELETE trigger should remove it from FTS index
    diesel::sql_query("DELETE FROM docs WHERE id = 1").execute(&mut conn)?;
    let rows = diesel::sql_query(
        "SELECT docs.id FROM docs JOIN docs_fts ON docs.rowid = docs_fts.rowid \
         WHERE docs_fts MATCH 'hello'",
    )
    .load::<Row>(&mut conn)?;
    let ids: Vec<i32> = rows.into_iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![3], "After deleting id=1, only id=3 should match 'hello'");

    // Update row 3 body — UPDATE trigger should refresh the FTS index
    diesel::sql_query("UPDATE docs SET body = 'goodbye SQLite' WHERE id = 3").execute(&mut conn)?;
    let rows = diesel::sql_query(
        "SELECT docs.id FROM docs JOIN docs_fts ON docs.rowid = docs_fts.rowid \
         WHERE docs_fts MATCH 'hello'",
    )
    .load::<Row>(&mut conn)?;
    assert!(rows.is_empty(), "After updating id=3, no rows should match 'hello'");

    Ok(())
}

#[test]
fn test_fts5_backfill_insert_is_generated() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, body TEXT);
        CREATE INDEX docs_fts ON docs USING GIN (to_tsvector('english', body));
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let out = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");

    // A backfill INSERT must be present after the triggers so pre-existing
    // rows are searchable immediately after index creation.
    assert!(
        out.contains("INSERT INTO") && out.contains("SELECT"),
        "Expected backfill INSERT ... SELECT: {out}"
    );
    // Execute all translated statements to verify the output is valid SQLite.
    {
        let conn = rusqlite::Connection::open_in_memory()?;
        for stmt in &translated {
            conn.execute_batch(&format!("{stmt};"))?;
        }
    }
    Ok(())
}

#[test]
fn test_fts5_backfill_populates_preexisting_rows() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, body TEXT);
        CREATE INDEX docs_fts ON docs USING GIN (to_tsvector('english', body));
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let mut conn = SqliteConnection::establish(":memory:")?;

    // Create the table FIRST, insert rows BEFORE creating the FTS index, then
    // run the remaining translated statements (FTS virtual table, triggers,
    // and the backfill INSERT).
    let stmts: Vec<_> = translated.iter().map(|s| s.to_string()).collect();

    // First statement is CREATE TABLE
    diesel::sql_query(&stmts[0]).execute(&mut conn)?;

    // Insert rows before the FTS infrastructure exists
    diesel::sql_query("INSERT INTO docs VALUES (1, 'hello world')").execute(&mut conn)?;
    diesel::sql_query("INSERT INTO docs VALUES (2, 'goodbye world')").execute(&mut conn)?;

    // Execute the remaining statements (FTS table, triggers, backfill)
    for stmt in &stmts[1..] {
        diesel::sql_query(stmt).execute(&mut conn)?;
    }

    // Both pre-existing rows must be searchable via FTS
    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
    }

    let rows = diesel::sql_query(
        "SELECT docs.id FROM docs JOIN docs_fts ON docs.rowid = docs_fts.rowid \
         WHERE docs_fts MATCH 'world'",
    )
    .load::<Row>(&mut conn)?;
    let mut ids: Vec<i32> = rows.into_iter().map(|r| r.id).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2], "Pre-existing rows should be searchable after backfill");

    Ok(())
}

#[test]
fn test_fts5_partial_index_generates_when_clause_in_triggers()
-> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE articles (id INT PRIMARY KEY, body TEXT, published BOOLEAN);
        CREATE INDEX articles_fts ON articles USING GIN (to_tsvector('english', body))
            WHERE published = true;
    ";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let out = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");

    // All three triggers must carry a WHEN clause derived from the predicate.
    let trigger_count_with_when =
        out.lines().filter(|l| l.contains("CREATE TRIGGER") && l.contains("WHEN")).count();
    assert_eq!(
        trigger_count_with_when, 3,
        "All 3 FTS5 sync triggers must have a WHEN clause for partial index: {out}"
    );

    // The predicate expression should appear inside the WHEN clause.
    assert!(
        out.contains("WHEN") && out.contains("published"),
        "Expected 'WHEN published' in trigger output: {out}"
    );
    // Execute all translated statements to verify the output is valid SQLite.
    {
        let conn = rusqlite::Connection::open_in_memory()?;
        for stmt in &translated {
            conn.execute_batch(&format!("{stmt};"))?;
        }
    }
    Ok(())
}
