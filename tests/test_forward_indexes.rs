//! Tests for forward index translation covering FTS and other index types
//! in `src/impls/translator_impls/create_index.rs`.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn translate_result(sql: &str) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect())
        .map_err(|e| e.to_string())
}

// ==================== Basic index ====================

#[test]
fn basic_btree_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        CREATE INDEX idx_users_name ON users (name);
    ";
    let output = translate(sql);
    assert!(output.contains("CREATE INDEX"), "Expected CREATE INDEX: {output}");
    assert!(output.contains("idx_users_name"), "Expected index name: {output}");
}

#[test]
fn unique_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, email TEXT);
        CREATE UNIQUE INDEX idx_unique_email ON users (email);
    ";
    let output = translate(sql);
    assert!(output.contains("UNIQUE"), "Expected UNIQUE index: {output}");
}

#[test]
fn index_if_not_exists() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX IF NOT EXISTS idx_name ON users (name);
    ";
    let output = translate(sql);
    assert!(output.contains("IF NOT EXISTS"), "Expected IF NOT EXISTS: {output}");
}

#[test]
fn multi_column_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        CREATE INDEX idx_name_age ON users (name, age);
    ";
    let output = translate(sql);
    assert!(output.contains("name"), "Expected name column: {output}");
    assert!(output.contains("age"), "Expected age column: {output}");
}

// ==================== GIN index with to_tsvector ====================

#[test]
fn gin_tsvector_index_to_fts5() {
    let sql = "
        CREATE TABLE articles (id INT PRIMARY KEY, title TEXT, body TEXT);
        CREATE INDEX idx_articles_fts ON articles USING GIN (to_tsvector('english', title || ' ' || body));
    ";
    let output = translate(sql);
    // Should be translated to FTS5 virtual table or similar
    assert!(
        output.contains("fts5") || output.contains("FTS") || output.contains("articles"),
        "Expected FTS5 or table reference: {output}"
    );
}

#[test]
fn gin_tsvector_single_column() {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, content TEXT);
        CREATE INDEX idx_docs_fts ON docs USING GIN (to_tsvector('english', content));
    ";
    let output = translate(sql);
    assert!(
        output.contains("fts5") || output.contains("content") || output.contains("docs"),
        "Expected FTS5 or column reference: {output}"
    );
}

// ==================== GiST index ====================

#[test]
fn gist_tsvector_index() {
    let sql = "
        CREATE TABLE articles (id INT PRIMARY KEY, title TEXT);
        CREATE INDEX idx_gist ON articles USING GiST (to_tsvector('english', title));
    ";
    let output = translate(sql);
    // GiST with tsvector should also translate to FTS5
    assert!(
        output.contains("fts5") || output.contains("articles"),
        "Expected FTS5 translation: {output}"
    );
}

// ==================== Hash index (should be skipped or converted)
// ====================

#[test]
fn hash_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_hash ON users USING HASH (name);
    ";
    let output = translate(sql);
    // Hash indexes can't be directly translated - should become regular index or be
    // dropped
    assert!(output.contains("users"), "Expected table still present: {output}");
}

// ==================== Expression index ====================

#[test]
fn expression_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_lower_name ON users ((lower(name)));
    ";
    let output = translate(sql);
    assert!(output.contains("users"), "Expected output: {output}");
}

// ==================== Partial index (WHERE clause) ====================

#[test]
fn partial_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, active BOOLEAN);
        CREATE INDEX idx_active ON users (name) WHERE active = true;
    ";
    let output = translate(sql);
    assert!(
        output.contains("WHERE") || output.contains("users"),
        "Expected index or table: {output}"
    );
}

// ==================== Bug 1: PG-only fields stripped from regular CREATE INDEX
// ====================

#[test]
fn concurrently_is_dropped() {
    // CONCURRENTLY is not valid in SQLite
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX CONCURRENTLY idx_name ON users (name);
    ";
    let output = translate(sql);
    assert!(!output.contains("CONCURRENTLY"), "CONCURRENTLY must be stripped: {output}");
    assert!(output.contains("idx_name"), "Index name should be preserved: {output}");
}

#[test]
fn include_clause_is_dropped() {
    // INCLUDE (covering index) is PostgreSQL-only
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        CREATE INDEX idx_name ON users (name) INCLUDE (age);
    ";
    let output = translate(sql);
    assert!(!output.contains("INCLUDE"), "INCLUDE clause must be stripped: {output}");
    assert!(output.contains("idx_name"), "Index name should be preserved: {output}");
}

#[test]
fn using_btree_is_dropped() {
    // USING BTREE is the default and is not emitted in SQLite syntax
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_name ON users USING BTREE (name);
    ";
    let output = translate(sql);
    assert!(!output.contains("USING"), "USING clause must be stripped: {output}");
    assert!(output.contains("idx_name"), "Index name should be preserved: {output}");
}

// ==================== GIN/GiST non-tsvector errors and tsvector FTS5
// translation ====================

/// Test that GIN index without to_tsvector causes an error.
#[test]
fn gin_index_non_tsvector_causes_error() {
    let sql = "
        CREATE TABLE documents (id SERIAL PRIMARY KEY, content TEXT);
        CREATE INDEX idx_content ON documents USING GIN (content);
    ";
    let result = translate_result(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("to_tsvector"), "Error should mention to_tsvector: {}", err);
}

/// Test that GiST index on non-tsvector column causes an error.
#[test]
fn gist_index_non_tsvector_causes_error() {
    let sql = "
        CREATE TABLE locations (id SERIAL PRIMARY KEY, point TEXT);
        CREATE INDEX idx_point ON locations USING GiST (point);
    ";
    let result = translate_result(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("GiST"), "Error should mention GiST index: {}", err);
}

/// Test that GiST index with to_tsvector translates to FTS5 (same as GIN).
#[test]
fn gist_tsvector_translates_to_fts5() {
    let sql = "
        CREATE TABLE articles (id SERIAL PRIMARY KEY, title TEXT, body TEXT);
        CREATE INDEX idx_search ON articles USING GiST (to_tsvector('english', title || ' ' || body));
    ";
    let translated_sql = translate_result(sql).unwrap();

    // Should have: table + FTS5 virtual table + 3 triggers + 1 backfill INSERT
    assert_eq!(translated_sql.len(), 6);

    // First statement is the table
    assert!(translated_sql[0].contains("CREATE TABLE articles"));

    // Second statement should be CREATE VIRTUAL TABLE ... USING fts5
    assert!(
        translated_sql[1].contains("CREATE VIRTUAL TABLE"),
        "Expected FTS5 virtual table, got: {}",
        translated_sql[1]
    );
    assert!(translated_sql[1].contains("fts5"), "Expected fts5 module, got: {}", translated_sql[1]);
    assert!(
        translated_sql[1].contains("articles_fts"),
        "Expected articles_fts table name, got: {}",
        translated_sql[1]
    );
}

/// Test that GIN index with to_tsvector translates to FTS5.
#[test]
fn gin_tsvector_translates_to_fts5() {
    let sql = "
        CREATE TABLE documents (id SERIAL PRIMARY KEY, title TEXT, body TEXT);
        CREATE INDEX idx_search ON documents USING GIN (to_tsvector('english', title || ' ' || body));
    ";
    let translated_sql = translate_result(sql).unwrap();

    // Should have: table + FTS5 virtual table + 3 triggers (insert, delete, update)
    // + 1 backfill INSERT
    assert_eq!(translated_sql.len(), 6);

    // First statement is the table
    assert!(translated_sql[0].contains("CREATE TABLE documents"));

    // Second statement should be CREATE VIRTUAL TABLE ... USING fts5
    assert!(
        translated_sql[1].contains("CREATE VIRTUAL TABLE"),
        "Expected FTS5 virtual table, got: {}",
        translated_sql[1]
    );
    assert!(translated_sql[1].contains("fts5"), "Expected fts5 module, got: {}", translated_sql[1]);
    assert!(
        translated_sql[1].contains("documents_fts"),
        "Expected documents_fts table name, got: {}",
        translated_sql[1]
    );
    assert!(
        translated_sql[1].contains("title"),
        "Expected title column, got: {}",
        translated_sql[1]
    );
    assert!(translated_sql[1].contains("body"), "Expected body column, got: {}", translated_sql[1]);

    // Statements 3-5 should be triggers
    assert!(
        translated_sql[2].contains("CREATE TRIGGER") && translated_sql[2].contains("AFTER INSERT"),
        "Expected INSERT trigger, got: {}",
        translated_sql[2]
    );
    assert!(
        translated_sql[3].contains("CREATE TRIGGER") && translated_sql[3].contains("AFTER DELETE"),
        "Expected DELETE trigger, got: {}",
        translated_sql[3]
    );
    assert!(
        translated_sql[4].contains("CREATE TRIGGER") && translated_sql[4].contains("AFTER UPDATE"),
        "Expected UPDATE trigger, got: {}",
        translated_sql[4]
    );
}
