//! Tests for reverse DELETE, UPDATE, and INSERT paths.
//!
//! Covers uncovered code in:
//! - `src/impls/reverse_translator_impls/delete.rs` (RETURNING, FROM clause,
//!   Derived table)
//! - `src/impls/reverse_translator_impls/update.rs` (RETURNING, complex SET,
//!   joins, Derived)
//! - `src/impls/reverse_translator_impls/insert.rs` (OR ROLLBACK/ABORT/FAIL, ON
//!   CONFLICT DO UPDATE + WHERE, RETURNING)

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn reverse(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();
    assert!(!stmts.is_empty(), "Expected at least one statement");
    stmts[0].to_string()
}

fn reverse_err(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    translator.reverse_sql(sqlite_sql, &schema, &options).unwrap_err().to_string()
}

const SCHEMA: &str = "CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);";
const TWO_TABLES: &str = "
    CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
    CREATE TABLE posts (id INT PRIMARY KEY, user_id INT, title TEXT);
";

// =============================================================================
// DELETE tests
// =============================================================================

#[test]
fn reverse_delete_basic() {
    let pg = reverse(SCHEMA, "DELETE FROM users WHERE id = 1;");
    assert!(pg.contains("DELETE FROM users"), "Expected DELETE FROM users: {pg}");
    assert!(pg.contains("WHERE"), "Expected WHERE: {pg}");
}

#[test]
fn reverse_delete_with_subquery_in_where() {
    let pg = reverse(
        TWO_TABLES,
        "DELETE FROM users WHERE id IN (SELECT user_id FROM posts WHERE title = 'test');",
    );
    assert!(pg.contains("DELETE"), "Expected DELETE: {pg}");
    assert!(pg.contains("IN (SELECT"), "Expected IN subquery: {pg}");
}

#[test]
fn reverse_delete_all_rows() {
    let pg = reverse(SCHEMA, "DELETE FROM users;");
    assert!(pg.contains("DELETE FROM users"), "Expected DELETE FROM users: {pg}");
}

// =============================================================================
// UPDATE tests
// =============================================================================

#[test]
fn reverse_update_basic() {
    let pg = reverse(SCHEMA, "UPDATE users SET name = 'test' WHERE id = 1;");
    assert!(pg.contains("UPDATE users SET"), "Expected UPDATE: {pg}");
    assert!(pg.contains("name ="), "Expected SET name: {pg}");
}

#[test]
fn reverse_update_complex_set_expr() {
    let pg = reverse(SCHEMA, "UPDATE users SET age = age + 1 WHERE id = 1;");
    assert!(pg.contains("UPDATE"), "Expected UPDATE: {pg}");
    assert!(pg.contains("age + 1") || pg.contains("age ="), "Expected expression: {pg}");
}

#[test]
fn reverse_update_multiple_columns() {
    let pg = reverse(SCHEMA, "UPDATE users SET name = 'Bob', age = 30 WHERE id = 1;");
    assert!(pg.contains("name ="), "Expected name SET: {pg}");
    assert!(pg.contains("age ="), "Expected age SET: {pg}");
}

// =============================================================================
// INSERT tests
// =============================================================================

#[test]
fn reverse_insert_basic() {
    let pg = reverse(SCHEMA, "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30);");
    assert!(pg.contains("INSERT INTO users"), "Expected INSERT: {pg}");
}

#[test]
fn reverse_insert_or_ignore() {
    let pg =
        reverse(SCHEMA, "INSERT OR IGNORE INTO users (id, name, age) VALUES (1, 'Alice', 30);");
    assert!(pg.contains("ON CONFLICT"), "Expected ON CONFLICT: {pg}");
    assert!(pg.contains("DO NOTHING"), "Expected DO NOTHING: {pg}");
}

#[test]
fn reverse_insert_or_replace() {
    let pg =
        reverse(SCHEMA, "INSERT OR REPLACE INTO users (id, name, age) VALUES (1, 'Alice', 30);");
    assert!(pg.contains("ON CONFLICT"), "Expected ON CONFLICT: {pg}");
    assert!(pg.contains("DO UPDATE SET"), "Expected DO UPDATE SET: {pg}");
}

#[test]
fn reverse_insert_or_rollback() {
    // INSERT OR ROLLBACK has no direct PG equivalent, should pass through without
    // ON CONFLICT
    let pg =
        reverse(SCHEMA, "INSERT OR ROLLBACK INTO users (id, name, age) VALUES (1, 'Alice', 30);");
    assert!(pg.contains("INSERT"), "Expected INSERT: {pg}");
}

#[test]
fn reverse_insert_or_abort() {
    let pg = reverse(SCHEMA, "INSERT OR ABORT INTO users (id, name, age) VALUES (1, 'Alice', 30);");
    assert!(pg.contains("INSERT"), "Expected INSERT: {pg}");
}

#[test]
fn reverse_insert_or_fail() {
    let pg = reverse(SCHEMA, "INSERT OR FAIL INTO users (id, name, age) VALUES (1, 'Alice', 30);");
    assert!(pg.contains("INSERT"), "Expected INSERT: {pg}");
}

#[test]
fn reverse_insert_or_replace_missing_pk_error() {
    let err = reverse_err(SCHEMA, "INSERT OR REPLACE INTO users (name, age) VALUES ('Alice', 30);");
    assert!(
        err.contains("primary key") || err.contains("Primary"),
        "Expected missing PK error: {err}"
    );
}

#[test]
fn reverse_insert_from_select() {
    let pg =
        reverse(TWO_TABLES, "INSERT INTO users (id, name, age) SELECT id, title, 0 FROM posts;");
    assert!(pg.contains("INSERT INTO"), "Expected INSERT INTO: {pg}");
    assert!(pg.contains("SELECT"), "Expected SELECT: {pg}");
}

#[test]
fn reverse_insert_on_conflict_do_update_with_where() {
    // Test ON CONFLICT ... DO UPDATE SET ... WHERE clause
    let pg = reverse(
        SCHEMA,
        "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30) ON CONFLICT (id) DO UPDATE SET name = excluded.name WHERE users.age > 18;",
    );
    assert!(pg.contains("ON CONFLICT"), "Expected ON CONFLICT: {pg}");
    assert!(pg.contains("DO UPDATE SET"), "Expected DO UPDATE SET: {pg}");
}

// =============================================================================
// DELETE with derived table in WHERE
// =============================================================================

#[test]
fn reverse_delete_with_derived_subquery() {
    let pg = reverse(
        TWO_TABLES,
        "DELETE FROM users WHERE id IN (SELECT sub.user_id FROM (SELECT user_id FROM posts WHERE title = 'test') AS sub);",
    );
    assert!(pg.contains("DELETE"), "Expected DELETE: {pg}");
    assert!(pg.contains("IN"), "Expected IN: {pg}");
}

// =============================================================================
// UPDATE with complex expressions
// =============================================================================

#[test]
fn reverse_update_with_subquery_in_where() {
    let pg = reverse(
        TWO_TABLES,
        "UPDATE users SET name = 'active' WHERE id IN (SELECT user_id FROM posts);",
    );
    assert!(pg.contains("UPDATE"), "Expected UPDATE: {pg}");
    assert!(pg.contains("IN (SELECT"), "Expected IN subquery: {pg}");
}

#[test]
fn reverse_update_with_join() {
    let pg = reverse(
        TWO_TABLES,
        "UPDATE users SET name = posts.title FROM posts WHERE users.id = posts.user_id;",
    );
    assert!(pg.contains("UPDATE"), "Expected UPDATE: {pg}");
}

// =============================================================================
// INSERT with complex source
// =============================================================================

#[test]
fn reverse_insert_from_select_with_join() {
    let pg = reverse(
        TWO_TABLES,
        "INSERT INTO users (id, name, age) SELECT p.id, p.title, 0 FROM posts p;",
    );
    assert!(pg.contains("INSERT"), "Expected INSERT: {pg}");
    assert!(pg.contains("SELECT"), "Expected SELECT: {pg}");
}

// =============================================================================
// DELETE with RETURNING
// =============================================================================

#[test]
fn reverse_delete_with_returning() {
    let pg = reverse(SCHEMA, "DELETE FROM users WHERE id = 1 RETURNING *;");
    assert!(pg.contains("DELETE"), "Expected DELETE: {pg}");
    assert!(pg.contains("RETURNING"), "Expected RETURNING: {pg}");
}

#[test]
fn reverse_delete_with_returning_column() {
    let pg = reverse(SCHEMA, "DELETE FROM users WHERE id = 1 RETURNING id, name;");
    assert!(pg.contains("RETURNING"), "Expected RETURNING: {pg}");
}

#[test]
fn reverse_delete_with_returning_alias() {
    let pg = reverse(SCHEMA, "DELETE FROM users WHERE id = 1 RETURNING id AS deleted_id;");
    assert!(pg.contains("RETURNING"), "Expected RETURNING: {pg}");
    assert!(pg.contains("deleted_id"), "Expected alias: {pg}");
}

#[test]
fn reverse_delete_order_by_and_limit_translate_expressions() {
    let pg = reverse(
        SCHEMA,
        "DELETE FROM users WHERE id > 0 ORDER BY datetime('now') LIMIT datetime('now');",
    );
    assert!(
        pg.contains("ORDER BY NOW()"),
        "Expected ORDER BY expression reverse translation: {pg}"
    );
    assert!(pg.contains("LIMIT NOW()"), "Expected LIMIT expression reverse translation: {pg}");
}

// =============================================================================
// UPDATE with RETURNING
// =============================================================================

#[test]
fn reverse_update_with_returning() {
    let pg = reverse(SCHEMA, "UPDATE users SET name = 'test' WHERE id = 1 RETURNING *;");
    assert!(pg.contains("UPDATE"), "Expected UPDATE: {pg}");
    assert!(pg.contains("RETURNING"), "Expected RETURNING: {pg}");
}

#[test]
fn reverse_update_with_returning_alias() {
    let pg = reverse(
        SCHEMA,
        "UPDATE users SET name = 'test' WHERE id = 1 RETURNING name AS updated_name;",
    );
    assert!(pg.contains("RETURNING"), "Expected RETURNING: {pg}");
    assert!(pg.contains("updated_name"), "Expected alias: {pg}");
}

#[test]
fn reverse_update_limit_translate_expression() {
    let pg = reverse(SCHEMA, "UPDATE users SET name = 'test' WHERE id > 0 LIMIT datetime('now');");
    assert!(pg.contains("LIMIT NOW()"), "Expected LIMIT expression reverse translation: {pg}");
}

// =============================================================================
// UPDATE with FROM clause
// =============================================================================

#[test]
fn reverse_update_with_from() {
    let pg = reverse(
        TWO_TABLES,
        "UPDATE users SET name = posts.title FROM posts WHERE users.id = posts.user_id;",
    );
    assert!(pg.contains("UPDATE"), "Expected UPDATE: {pg}");
    assert!(pg.contains("FROM"), "Expected FROM: {pg}");
}

// =============================================================================
// INSERT with RETURNING
// =============================================================================

#[test]
fn reverse_insert_with_returning() {
    let pg =
        reverse(SCHEMA, "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30) RETURNING *;");
    assert!(pg.contains("INSERT"), "Expected INSERT: {pg}");
    assert!(pg.contains("RETURNING"), "Expected RETURNING: {pg}");
}

#[test]
fn reverse_insert_with_returning_alias() {
    let pg = reverse(
        SCHEMA,
        "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30) RETURNING id AS new_id;",
    );
    assert!(pg.contains("RETURNING"), "Expected RETURNING: {pg}");
    assert!(pg.contains("new_id"), "Expected alias: {pg}");
}

// =============================================================================
// DELETE with USING clause (reverse translate)
// =============================================================================

#[test]
fn reverse_delete_with_using() {
    let pg = reverse(TWO_TABLES, "DELETE FROM users WHERE id IN (SELECT user_id FROM posts);");
    assert!(pg.contains("DELETE"), "Expected DELETE: {pg}");
}

// =============================================================================
// DELETE with join in FROM
// =============================================================================

#[test]
fn reverse_delete_with_join_in_where() {
    let pg = reverse(
        TWO_TABLES,
        "DELETE FROM users WHERE EXISTS (SELECT 1 FROM posts WHERE posts.user_id = users.id);",
    );
    assert!(pg.contains("DELETE"), "Expected DELETE: {pg}");
    assert!(pg.contains("EXISTS"), "Expected EXISTS: {pg}");
}

// =============================================================================
// UPDATE with derived table in FROM
// =============================================================================

#[test]
fn reverse_update_with_derived_table() {
    let pg = reverse(
        TWO_TABLES,
        "UPDATE users SET name = sub.title FROM (SELECT user_id, title FROM posts) AS sub WHERE users.id = sub.user_id;",
    );
    assert!(pg.contains("UPDATE"), "Expected UPDATE: {pg}");
}

// =============================================================================
// INSERT with ON CONFLICT DO NOTHING
// =============================================================================

#[test]
fn reverse_insert_on_conflict_do_nothing() {
    let pg = reverse(
        SCHEMA,
        "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30) ON CONFLICT (id) DO NOTHING;",
    );
    assert!(pg.contains("ON CONFLICT"), "Expected ON CONFLICT: {pg}");
    assert!(pg.contains("DO NOTHING"), "Expected DO NOTHING: {pg}");
}

// =============================================================================
// INSERT with default values
// =============================================================================

#[test]
fn reverse_insert_multiple_rows() {
    let pg = reverse(
        SCHEMA,
        "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30), (2, 'Bob', 25);",
    );
    assert!(pg.contains("INSERT"), "Expected INSERT: {pg}");
    assert!(pg.contains("Alice"), "Expected Alice: {pg}");
    assert!(pg.contains("Bob"), "Expected Bob: {pg}");
}

// =============================================================================
// UPDATE with multiple SET and complex WHERE
// =============================================================================

#[test]
fn reverse_update_with_complex_where() {
    let pg = reverse(
        SCHEMA,
        "UPDATE users SET name = 'updated', age = 99 WHERE id > 5 AND name LIKE '%test%';",
    );
    assert!(pg.contains("UPDATE"), "Expected UPDATE: {pg}");
    assert!(pg.contains("LIKE"), "Expected LIKE: {pg}");
}

// =============================================================================
// UPDATE FROM with JOIN (covers reverse_translate_join in update.rs)
// =============================================================================

#[test]
fn reverse_update_from_with_join() {
    let pg = reverse(
        "CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
         CREATE TABLE posts (id INT PRIMARY KEY, user_id INT, title TEXT);
         CREATE TABLE tags (id INT PRIMARY KEY, post_id INT, tag TEXT);",
        "UPDATE users SET name = posts.title FROM posts JOIN tags ON posts.id = tags.post_id WHERE users.id = posts.user_id;",
    );
    assert!(pg.contains("UPDATE"), "Expected UPDATE: {pg}");
    assert!(pg.contains("FROM"), "Expected FROM: {pg}");
}

// =============================================================================
// INSERT with SELECT containing expressions
// =============================================================================

#[test]
fn reverse_insert_select_with_expression() {
    let pg = reverse(
        TWO_TABLES,
        "INSERT INTO users (id, name, age) SELECT id, title || ' author', 0 FROM posts;",
    );
    assert!(pg.contains("INSERT"), "Expected INSERT: {pg}");
    assert!(pg.contains("SELECT"), "Expected SELECT: {pg}");
}

// =============================================================================
// DELETE with returning expression
// =============================================================================

#[test]
fn reverse_delete_returning_expression() {
    let pg = reverse(
        SCHEMA,
        "DELETE FROM users WHERE age < 18 RETURNING id, name || ' deleted' AS msg;",
    );
    assert!(pg.contains("DELETE"), "Expected DELETE: {pg}");
    assert!(pg.contains("RETURNING"), "Expected RETURNING: {pg}");
}

// =============================================================================
// UPDATE with returning expression
// =============================================================================

#[test]
fn reverse_update_returning_expression() {
    let pg = reverse(
        SCHEMA,
        "UPDATE users SET age = age + 1 WHERE id = 1 RETURNING id, name, age AS new_age;",
    );
    assert!(pg.contains("RETURNING"), "Expected RETURNING: {pg}");
    assert!(pg.contains("new_age"), "Expected alias: {pg}");
}

// =============================================================================
// INSERT with RETURNING expression
// =============================================================================

#[test]
fn reverse_insert_returning_expression() {
    let pg = reverse(
        SCHEMA,
        "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30) RETURNING id, name AS inserted_name;",
    );
    assert!(pg.contains("RETURNING"), "Expected RETURNING: {pg}");
    assert!(pg.contains("inserted_name"), "Expected alias: {pg}");
}

// =============================================================================
// Bug 5: INSERT OR REPLACE on PK-only table must not produce empty DO UPDATE
// SET
// =============================================================================

#[test]
fn reverse_insert_or_replace_pk_only_table_falls_back_to_do_nothing() {
    // A table with only a primary key column has no non-PK columns, so there is
    // nothing to UPDATE.  The upsert must degrade to DO NOTHING rather than
    // emitting an empty (invalid) DO UPDATE SET clause.
    let schema = "CREATE TABLE tags (id INT PRIMARY KEY);";
    let pg = reverse(schema, "INSERT OR REPLACE INTO tags (id) VALUES (1);");
    assert!(pg.contains("ON CONFLICT"), "Expected ON CONFLICT: {pg}");
    assert!(pg.contains("DO NOTHING"), "PK-only upsert must fall back to DO NOTHING: {pg}");
    assert!(!pg.contains("DO UPDATE SET"), "Empty DO UPDATE SET must not be emitted: {pg}");
}

#[test]
fn reverse_insert_or_replace_multi_column_still_uses_do_update_set() {
    // Normal table with non-PK columns should still produce DO UPDATE SET.
    let pg =
        reverse(SCHEMA, "INSERT OR REPLACE INTO users (id, name, age) VALUES (1, 'Alice', 30);");
    assert!(pg.contains("DO UPDATE SET"), "Multi-column table should use DO UPDATE SET: {pg}");
}
