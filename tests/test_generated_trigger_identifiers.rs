//! Regression tests for trigger identifiers built through direct AST
//! construction.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[test]
fn fts5_generated_trigger_sql_handles_quoted_identifiers() {
    let sql = r#"
        CREATE TABLE docs (
            id INTEGER PRIMARY KEY,
            title TEXT NOT NULL,
            "body-text" TEXT NOT NULL
        );

        CREATE INDEX idx_docs_fts ON docs
            USING GIN (to_tsvector('english', title || ' ' || "body-text"));
    "#;

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_ok(), "Translation should succeed for quoted FTS5 identifiers");

    let translated = result.unwrap().into_iter().map(|stmt| stmt.to_string()).collect::<Vec<_>>();
    assert!(
        translated.iter().any(|sql| sql.contains("CREATE TRIGGER docs_fts_ai")),
        "Expected generated FTS5 trigger in translated SQL"
    );
    // Execute all translated statements to verify the output is valid SQLite.
    {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for stmt in &translated {
            conn.execute_batch(&format!("{stmt};")).expect("translated SQL must execute in SQLite");
        }
    }
}

#[test]
fn vec0_generated_trigger_sql_handles_quoted_identifiers() {
    let sql = r#"
        CREATE TABLE "embeddings-bad" (
            id INTEGER PRIMARY KEY,
            embedding vector(3)
        );
    "#;

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_ok(), "Translation should succeed for quoted vec0 identifiers");

    let translated = result.unwrap().into_iter().map(|stmt| stmt.to_string()).collect::<Vec<_>>();
    assert!(
        translated
            .iter()
            .any(|sql| sql.contains("CREATE TRIGGER \"embeddings-bad_embedding_vec_ai\"")),
        "Expected generated vec0 trigger in translated SQL"
    );
    // Execute translated DDL, skipping vec0 virtual tables that need the
    // sqlite-vec extension loaded (unavailable in the standard test runtime).
    {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for stmt in &translated {
            if stmt.contains("USING vec0") {
                continue;
            }
            conn.execute_batch(&format!("{stmt};")).expect("translated SQL must execute in SQLite");
        }
    }
}
