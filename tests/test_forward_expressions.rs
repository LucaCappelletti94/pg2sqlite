//! Tests for forward expression translation covering FTS and other paths
//! in `src/impls/translator_impls/expr.rs`.

#[path = "helpers/translate.rs"]
mod translate_helpers;
use translate_helpers::translate_default as translate;

const SCHEMA: &str = "
    CREATE TABLE articles (id INT PRIMARY KEY, title TEXT, body TEXT, category TEXT);
    CREATE INDEX idx_articles_fts ON articles USING GIN (to_tsvector('english', title || ' ' || body));
";

#[test]
fn tsvector_tsquery_match() {
    let sql = format!(
        "{SCHEMA}
         CREATE VIEW search_results AS
         SELECT * FROM articles WHERE to_tsvector('english', title || ' ' || body) @@ to_tsquery('english', 'hello & world');"
    );
    let output = translate(&sql);
    assert!(
        output.contains("MATCH") || output.contains("fts"),
        "Expected FTS5 MATCH translation: {output}"
    );
    apply_translated_pg(&sql);
}

#[test]
fn tsvector_tsquery_single_column() {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, content TEXT);
        CREATE INDEX idx_docs_fts ON docs USING GIN (to_tsvector('english', content));
        CREATE VIEW doc_search AS
        SELECT * FROM docs WHERE to_tsvector('english', content) @@ to_tsquery('search');
    ";
    let output = translate(sql);
    assert!(
        output.contains("MATCH") || output.contains("fts"),
        "Expected FTS5 translation: {output}"
    );
    apply_translated_pg(sql);
}

#[test]
fn view_with_case_expression() {
    let sql = "
        CREATE TABLE products (id INT PRIMARY KEY, price REAL, name TEXT);
        CREATE VIEW product_tiers AS
        SELECT name, CASE WHEN price > 100 THEN 'expensive' WHEN price > 50 THEN 'mid' ELSE 'cheap' END AS tier
        FROM products;
    ";
    let output = translate(sql);
    assert!(output.contains("CASE"), "Expected CASE: {output}");
    assert!(output.contains("WHEN"), "Expected WHEN: {output}");
    apply_translated_pg(sql);
}

#[test]
fn view_with_coalesce() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, nickname TEXT, name TEXT);
        CREATE VIEW display_names AS
        SELECT COALESCE(nickname, name, 'anonymous') AS display_name FROM users;
    ";
    let output = translate(sql);
    assert!(output.contains("COALESCE"), "Expected COALESCE: {output}");
    apply_translated_pg(sql);
}

#[test]
fn view_with_cast() {
    let sql = "
        CREATE TABLE events (id INT PRIMARY KEY, count_text TEXT);
        CREATE VIEW event_counts AS
        SELECT id, CAST(count_text AS INT) AS count_num FROM events;
    ";
    let output = translate(sql);
    assert!(output.contains("CAST"), "Expected CAST: {output}");
    apply_translated_pg(sql);
}

#[test]
fn view_with_between() {
    let sql = "
        CREATE TABLE items (id INT PRIMARY KEY, price REAL);
        CREATE VIEW mid_price AS
        SELECT * FROM items WHERE price BETWEEN 10.0 AND 100.0;
    ";
    let output = translate(sql);
    assert!(output.contains("BETWEEN"), "Expected BETWEEN: {output}");
    apply_translated_pg(sql);
}

#[test]
fn view_with_in_list() {
    let sql = "
        CREATE TABLE items (id INT PRIMARY KEY, category TEXT);
        CREATE VIEW filtered AS
        SELECT * FROM items WHERE category IN ('electronics', 'books', 'clothing');
    ";
    let output = translate(sql);
    assert!(output.contains("IN ("), "Expected IN list: {output}");
    apply_translated_pg(sql);
}

#[test]
fn view_with_exists_subquery() {
    let sql = "
        CREATE TABLE orders (id INT PRIMARY KEY, user_id INT);
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE VIEW users_with_orders AS
        SELECT * FROM users WHERE EXISTS (SELECT 1 FROM orders WHERE orders.user_id = users.id);
    ";
    let output = translate(sql);
    assert!(output.contains("EXISTS"), "Expected EXISTS: {output}");
    apply_translated_pg(sql);
}

#[test]
fn view_with_in_subquery() {
    let sql = "
        CREATE TABLE orders (id INT PRIMARY KEY, user_id INT);
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE VIEW active_users AS
        SELECT * FROM users WHERE id IN (SELECT user_id FROM orders);
    ";
    let output = translate(sql);
    assert!(output.contains("IN (SELECT"), "Expected IN subquery: {output}");
    apply_translated_pg(sql);
}

#[test]
fn view_with_is_null() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, deleted_at TEXT);
        CREATE VIEW active_users AS
        SELECT * FROM users WHERE deleted_at IS NULL;
    ";
    let output = translate(sql);
    assert!(output.contains("IS NULL"), "Expected IS NULL: {output}");
    apply_translated_pg(sql);
}

#[test]
fn view_with_like() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE VIEW matched_users AS
        SELECT * FROM users WHERE name LIKE '%test%';
    ";
    let output = translate(sql);
    assert!(output.contains("LIKE"), "Expected LIKE: {output}");
    apply_translated_pg(sql);
}

#[test]
fn view_with_not_exists() {
    let sql = "
        CREATE TABLE orders (id INT PRIMARY KEY, user_id INT);
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE VIEW no_orders AS
        SELECT * FROM users WHERE NOT EXISTS (SELECT 1 FROM orders WHERE orders.user_id = users.id);
    ";
    let output = translate(sql);
    assert!(output.contains("NOT EXISTS"), "Expected NOT EXISTS: {output}");
    apply_translated_pg(sql);
}

#[test]
fn view_with_json_access_operator_arrow() {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, payload JSONB);
        CREATE VIEW doc_keys AS
        SELECT payload->'author' AS author_json
        FROM docs;
    ";
    let output = translate(sql);
    assert!(output.contains("->"), "Expected JSON access operator in output: {output}");
    apply_translated_pg(sql);
}

#[test]
fn view_with_json_access_operator_arrow_text() {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, payload JSONB);
        CREATE VIEW doc_authors AS
        SELECT payload->>'author' AS author_text
        FROM docs;
    ";
    let output = translate(sql);
    assert!(output.contains("->>"), "Expected JSON text access operator in output: {output}");
    apply_translated_pg(sql);
}

#[test]
fn view_with_chained_json_access_operators() {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, payload JSONB);
        CREATE VIEW doc_author_names AS
        SELECT payload->'author'->>'name' AS author_name
        FROM docs;
    ";
    let output = translate(sql);
    assert!(
        output.contains("->'author'") || output.contains("-> 'author'"),
        "Expected chained JSON access output: {output}"
    );
    assert!(output.contains("->>"), "Expected chained JSON text access output: {output}");
    apply_translated_pg(sql);
}

/// Translates `pg` with default options and executes every emitted statement
/// against an in-memory SQLite connection.
/// Translated DDL cannot be expressed via diesel's typed DSL, so sql_query is
/// used here to prove the emitted SQL is accepted by SQLite.
fn apply_translated_pg(pg: &str) {
    use diesel::prelude::*;
    use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
    let stmts = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate");
    let mut conn = diesel::SqliteConnection::establish(":memory:").expect("in-memory connection");
    for s in &stmts {
        diesel::sql_query(s.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("translated statement must execute: {e}\n{s}"));
    }
}
