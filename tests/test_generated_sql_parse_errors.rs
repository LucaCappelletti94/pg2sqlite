//! Regression tests for generated SQL parse failures.
//!
//! Translators that synthesize SQL snippets (FTS5/vec0 triggers) must return
//! explicit errors when those snippets are invalid instead of silently dropping
//! statements.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[test]
fn fts5_generated_trigger_parse_failure_returns_error() {
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
    assert!(result.is_err(), "Translation should fail when generated FTS5 trigger SQL is invalid");

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Failed to parse generated FTS5 trigger SQL"),
        "Expected explicit FTS5 generated-SQL parse error, got: {error_msg}"
    );
}

#[test]
fn vec0_generated_trigger_parse_failure_returns_error() {
    let sql = r#"
        CREATE TABLE "embeddings-bad" (
            id INTEGER PRIMARY KEY,
            embedding vector(3)
        );
    "#;

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "Translation should fail when generated vec0 trigger SQL is invalid");

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Failed to parse generated vec0 synchronization trigger SQL"),
        "Expected explicit vec0 generated-SQL parse error, got: {error_msg}"
    );
}
