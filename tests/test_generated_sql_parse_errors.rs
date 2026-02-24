//! Regression tests for generated SQL snippet handling.
//!
//! Translators that synthesize SQL snippets (FTS5/vec0 triggers) must either
//! fail loudly on invalid SQL or produce valid SQL for quoted/problematic
//! identifiers.

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
}
