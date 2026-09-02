//! Red test: `to_tsvector(...) @@ to_tsquery(...)` predicates must
//! NOT silently emit a SQLite SELECT that references an FTS5 virtual
//! table the schema never declared. Today pg2sqlite happily rewrites
//! the predicate into `... IN (SELECT rowid FROM <name>_fts WHERE
//! <name>_fts MATCH '...')` even when no `CREATE INDEX ... USING
//! GIN (to_tsvector(...))` exists for the column, producing SQL that
//! runtime-errors with `no such table: <name>_fts`.
//!
//! The expected behaviour is one of:
//!
//! - error at translate time (preferred): "no FTS5 index over
//!   `<table>.<column>`; declare `CREATE INDEX ... USING GIN
//!   (to_tsvector(...))` to enable the rewrite."
//! - leave the predicate untranslated, so SQLite errors on the unknown
//!   `to_tsvector` / `to_tsquery` calls, which still surfaces the gap clearly.
//!
//! Either outcome would help the user; the current silent rewrite
//! against a missing table does not. This test passes when the
//! translator rejects the predicate AT TRANSLATE TIME, and fails when
//! it silently emits a rewrite (the current behaviour) or when the
//! emitted SQL applies cleanly against an FTS5 vtable that has not
//! been declared.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};

const SCHEMA_WITHOUT_GIN_INDEX: &str = "\
CREATE TABLE docs (
    id INTEGER PRIMARY KEY,
    body TEXT
);
INSERT INTO docs (id, body) VALUES (1, 'pg2sqlite test');
SELECT id FROM docs WHERE to_tsvector('english', body) @@ to_tsquery('test');
";

fn opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob)
}

#[test]
fn fts_predicate_without_gin_index_errors_at_translate_time() {
    let result =
        Pg2Sqlite::default().sql(SCHEMA_WITHOUT_GIN_INDEX).and_then(|t| t.translate(&opts()));

    // `Err(_)` is acceptable - the translator refused, which is the
    // expected behaviour. If translation succeeded, the output must
    // not silently reference an undeclared `_fts` vtable.
    if let Ok(stmts) = result {
        let joined = stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n");
        assert!(
            !joined.contains("_fts"),
            "translator silently rewrote a `@@ to_tsquery` predicate against an undeclared FTS5 \
             vtable. The schema did not declare a `CREATE INDEX ... USING GIN`, so the rewrite \
             must error at translate time (or pass the predicate through). Got:\n{joined}"
        );
        // Translated SQL is dynamically generated; rusqlite execute_batch
        // proves SQLite accepts it.
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
        for s in &stmts {
            conn.execute_batch(&format!("{s};"))
                .unwrap_or_else(|e| panic!("SQLite rejected emitted SQL: {e}\n{s}"));
        }
    }
}

#[test]
fn fts_predicate_with_matching_gin_index_still_translates() {
    // Positive control: the gate must NOT regress the happy path. When a
    // matching GIN over to_tsvector IS declared, the rewrite should still
    // produce the canonical FTS5 IN-subquery shape.
    let schema = "\
CREATE TABLE docs (
    id INTEGER PRIMARY KEY,
    body TEXT
);
CREATE INDEX docs_body_fts ON docs USING GIN (to_tsvector('english', body));
SELECT id FROM docs WHERE to_tsvector('english', body) @@ to_tsquery('test');
";
    let stmts = Pg2Sqlite::default()
        .sql(schema)
        .expect("parse")
        .translate(&opts())
        .expect("translate must succeed when a matching GIN index is declared");
    let joined = stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n");
    assert!(
        joined.contains("docs_fts MATCH 'test'"),
        "expected FTS5 MATCH rewrite, got:\n{joined}"
    );
    // Translated SQL is dynamically generated; rusqlite execute_batch proves
    // SQLite accepts it.
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("SQLite rejected emitted SQL: {e}\n{s}"));
    }
}

#[test]
fn fts_predicate_against_wrong_column_errors() {
    // GIN over `body` only. A query that matches `title` instead must error
    // with a message naming the actual column being matched, not silently
    // succeed against a non-existent vtable.
    let schema = "\
CREATE TABLE docs (
    id INTEGER PRIMARY KEY,
    title TEXT,
    body TEXT
);
CREATE INDEX docs_body_fts ON docs USING GIN (to_tsvector('english', body));
SELECT id FROM docs WHERE to_tsvector('english', title) @@ to_tsquery('test');
";
    let err =
        Pg2Sqlite::default().sql(schema).expect("parse").translate(&opts()).expect_err(
            "FTS rewrite against a column with no GIN index must error at translate time",
        );
    let msg = err.to_string();
    assert!(
        msg.contains("title") && msg.contains("not declared"),
        "expected error to name `title`, got: {msg}"
    );
}

#[test]
fn fts_index_catalog_is_case_insensitive() {
    // Schema uses upper-case column name; query uses lower-case. The
    // case-folded catalog must still match.
    let schema = "\
CREATE TABLE Docs (
    id INTEGER PRIMARY KEY,
    BODY TEXT
);
CREATE INDEX docs_body_fts ON Docs USING GIN (to_tsvector('english', BODY));
SELECT id FROM Docs WHERE to_tsvector('english', body) @@ to_tsquery('test');
";
    let stmts = Pg2Sqlite::default()
        .sql(schema)
        .expect("parse")
        .translate(&opts())
        .expect("case-insensitive catalog lookup must allow the rewrite");
    let joined = stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n");
    assert!(joined.contains("MATCH 'test'"), "expected FTS5 MATCH rewrite, got:\n{joined}");
    // Translated SQL is dynamically generated; rusqlite execute_batch proves
    // SQLite accepts it.
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("SQLite rejected emitted SQL: {e}\n{s}"));
    }
}
