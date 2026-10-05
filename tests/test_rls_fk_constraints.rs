// Test foreign key constraint behavior with RLS translation
//! This module tests that foreign key references are correctly updated when
//! tables with Row Level Security (RLS) are translated. When an RLS table is
//! split into a view and a backing table, all foreign keys must point to the
//! backing table (e.g., `users_rls`) instead of the view (e.g., `users`).

#![allow(clippy::format_collect)]

use std::cell::Cell;

use diesel::{prelude::*, sql_query};
use pg2sqlite::{
    options::Pg2SqliteOptions, pg2sqlite::Pg2Sqlite, prelude::UuidRepresentation,
    traits::SessionVariableMapping,
};
use rosetta_uuid::Uuid;

mod helpers;
use helpers::{Count, establish_connection, set_session_user_id};

diesel::define_sql_function! {
    /// Reports whether generated write guards are exempt.
    fn write_is_exempt() -> diesel::sql_types::Bool;
}

thread_local! {
    static WRITE_IS_EXEMPT: Cell<bool> = const { Cell::new(false) };
}
fn translation_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_session_variable(SessionVariableMapping::current_user("current_app_username"))
        .with_rls_audit_table_name("rls_audit".to_string())
}

#[test]
fn test_fk_points_to_rls_backing_table() -> Result<(), Box<dyn std::error::Error>> {
    let fixture_sql = include_str!("fixtures/rls_fk_simple.sql");
    let options = translation_options();

    // Translate the SQL
    let translated = Pg2Sqlite::default().sql(fixture_sql)?.translate(&options)?;

    let translated_sql: Vec<String> = translated.iter().map(ToString::to_string).collect();

    // Verify that the CREATE TABLE posts statement contains REFERENCES
    // users_rls, not REFERENCES users
    let posts_create = translated_sql
        .iter()
        .find(|s| s.contains("CREATE TABLE posts"))
        .expect("Should have CREATE TABLE posts");

    // The FK should point to users_rls (the backing table), not users (the
    // view)
    assert!(
        posts_create.contains("REFERENCES users_rls"),
        "FK should point to users_rls backing table, got: {posts_create}"
    );
    assert!(
        !posts_create.contains("REFERENCES users (") && !posts_create.contains("REFERENCES users("),
        "FK should not point to users view, got: {posts_create}"
    );

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for stmt in &translated {
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("emitted SQL failed: {e}\n{stmt}"));
    }

    Ok(())
}

#[test]
fn test_fk_constraint_enforcement() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    let fixture_sql = include_str!("fixtures/rls_fk_simple.sql");
    let options = translation_options();

    // Translate and execute
    let translated = Pg2Sqlite::default().sql(fixture_sql)?.translate(&options)?;

    for stmt in &translated {
        sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // Insert a valid user. With the deny-by-default + backing-table guard
    // trigger fix, direct backing-table inserts now hit the WITH CHECK
    // trigger too. Set the session user so `id = current_app_user()` passes.
    let user_id = Uuid::new_v4();
    let user_id_bytes = user_id.as_bytes();
    let user_id_hex = user_id_bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>();
    set_session_user_id(&user_id);
    sql_query(format!(
        "INSERT INTO users_rls (id, username, email) VALUES (x'{}', 'testuser', 'test@example.com')",
        user_id_hex
    ))
    .execute(&mut conn)?;

    // Try to insert a post with non-existent author_id - should fail with FK
    // violation
    let invalid_author_id = Uuid::new_v4();
    let invalid_id_bytes = invalid_author_id.as_bytes();
    let invalid_id_hex = invalid_id_bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>();
    let new_post_id = Uuid::new_v4();
    let new_post_id_bytes = new_post_id.as_bytes();
    let new_post_id_hex =
        new_post_id_bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>();
    let result = sql_query(format!(
        "INSERT INTO posts (id, author_id, title, content) VALUES (x'{}', x'{}', 'Test Post', 'Content')",
        new_post_id_hex,
        invalid_id_hex
    ))
    .execute(&mut conn);

    assert!(result.is_err(), "Insert with invalid FK should fail, but succeeded");

    // Insert a post with valid author_id - should succeed
    let post_id = Uuid::new_v4();
    let post_id_bytes = post_id.as_bytes();
    let post_id_hex = post_id_bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>();
    sql_query(format!(
        "INSERT INTO posts (id, author_id, title, content) VALUES (x'{}', x'{}', 'Valid Post', 'Content')",
        post_id_hex,
        user_id_hex
    ))
    .execute(&mut conn)?;

    // Verify the post was inserted
    let count: Count = sql_query("SELECT COUNT(*) as count FROM posts").get_result(&mut conn)?;
    assert_eq!(count.count, 1, "Post should be inserted");

    Ok(())
}

#[test]
fn test_fk_both_tables_rls() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    let fixture_sql = include_str!("fixtures/rls_fk_both.sql");
    let options = translation_options().with_write_exemption_function("write_is_exempt");
    write_is_exempt_utils::register_nondeterministic_impl(&mut conn, || {
        WRITE_IS_EXEMPT.with(Cell::get)
    })?;

    // Translate and execute
    let translated = Pg2Sqlite::default().sql(fixture_sql)?.translate(&options)?;

    let translated_sql: Vec<String> = translated.iter().map(ToString::to_string).collect();

    // Verify posts_rls references users_rls
    let posts_create = translated_sql
        .iter()
        .find(|s| s.contains("CREATE TABLE posts_rls"))
        .expect("Should have CREATE TABLE posts_rls");

    assert!(
        posts_create.contains("REFERENCES users_rls"),
        "FK should point to users_rls, got: {posts_create}"
    );

    for stmt in &translated {
        sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // Insert a user. The backing-table BEFORE INSERT guard trigger now
    // enforces the WITH CHECK predicate `id = current_app_user()`, so the
    // session user must match the row's id.
    let user_id = Uuid::new_v4();
    let user_id_bytes = user_id.as_bytes();
    let user_id_hex = user_id_bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>();
    set_session_user_id(&user_id);
    sql_query(format!(
        "INSERT INTO users_rls (id, username, email) VALUES (x'{}', 'alice', 'alice@example.com')",
        user_id_hex
    ))
    .execute(&mut conn)?;

    // Insert a post. posts has `WITH CHECK (author_id = current_app_user())`
    // so the session user must be the post's author. Reusing the same
    // user_id keeps both inserts consistent.
    let post_id = Uuid::new_v4();
    let post_id_bytes = post_id.as_bytes();
    let post_id_hex = post_id_bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>();
    sql_query(format!(
        "INSERT INTO posts_rls (id, author_id, title) VALUES (x'{}', x'{}', 'Alice Post')",
        post_id_hex, user_id_hex
    ))
    .execute(&mut conn)?;
    WRITE_IS_EXEMPT.with(|exempt| exempt.set(true));

    // Try to delete the user - should fail because post still references it
    let result = sql_query(format!("DELETE FROM users_rls WHERE id = x'{}'", user_id_hex))
        .execute(&mut conn);

    assert!(result.is_err(), "Delete should fail due to FK constraint");

    // Delete the post first
    sql_query(format!("DELETE FROM posts_rls WHERE id = x'{}'", post_id_hex)).execute(&mut conn)?;

    // Now deleting the user should succeed
    sql_query(format!("DELETE FROM users_rls WHERE id = x'{}'", user_id_hex)).execute(&mut conn)?;

    // Verify user is deleted
    let count: Count =
        sql_query("SELECT COUNT(*) as count FROM users_rls").get_result(&mut conn)?;
    assert_eq!(count.count, 0, "User should be deleted");

    Ok(())
}

#[test]
fn test_rls_fk_simple_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let fixture_sql = include_str!("fixtures/rls_fk_simple.sql");
    let options = translation_options();

    let translated = Pg2Sqlite::default().sql(fixture_sql)?.translate(&options)?;

    let translated_sql: Vec<String> = translated.iter().map(ToString::to_string).collect();

    // Verify each statement parses as valid SQLite
    for stmt in &translated {
        sqlparser::parser::Parser::parse_sql(
            &sqlparser::dialect::SQLiteDialect {},
            &stmt.to_string(),
        )
        .expect("Failed to parse as SQLite");
    }

    insta::assert_snapshot!("rls_fk_simple_translation", translated_sql.join(";\n"));
    Ok(())
}

#[test]
fn test_rls_fk_both_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let fixture_sql = include_str!("fixtures/rls_fk_both.sql");
    let options = translation_options();

    let translated = Pg2Sqlite::default().sql(fixture_sql)?.translate(&options)?;

    let translated_sql: Vec<String> = translated.iter().map(ToString::to_string).collect();

    // Verify each statement parses as valid SQLite
    for stmt in &translated {
        sqlparser::parser::Parser::parse_sql(
            &sqlparser::dialect::SQLiteDialect {},
            &stmt.to_string(),
        )
        .expect("Failed to parse as SQLite");
    }

    insta::assert_snapshot!("rls_fk_both_translation", translated_sql.join(";\n"));
    Ok(())
}
