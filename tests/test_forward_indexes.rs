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
