//! Test for grant-based table filtering in SQLite generation.
//!
//! Tables should be handled differently based on grants to `app_user`:
//! - No grants: Table should not be created in SQLite at all
//! - SELECT only: Table + view created, no write triggers (read-only sync)
//! - SELECT + write grants: Full RLS treatment with INSTEAD OF triggers
//!
//! ```text
//!                      ┌─────────────────────────────────────────────────────────────┐
//!                      │                    PostgreSQL Schema                        │
//!                      │                                                             │
//!                      │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐          │
//!                      │  │ audit_logs  │  │   users     │  │   posts     │          │
//!                      │  │             │  │             │  │             │          │
//!                      │  │ (no grants) │  │ (SELECT     │  │ (SELECT +   │          │
//!                      │  │             │  │  only)      │  │  INSERT +   │          │
//!                      │  │             │  │             │  │  UPDATE +   │          │
//!                      │  │             │  │             │  │  DELETE)    │          │
//!                      │  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘          │
//!                      └─────────┼────────────────┼────────────────┼─────────────────┘
//!                                │                │                │
//!                                ▼                ▼                ▼
//!                      ┌─────────────────────────────────────────────────────────────┐
//!                      │                 pg2sqlite Translation                       │
//!                      │              (with_session_user_role("app_user"))           │
//!                      └─────────────────────────────────────────────────────────────┘
//!                                │                │                │
//!                                ▼                ▼                ▼
//!                           ┌────────┐      ┌──────────┐     ┌──────────┐
//!                           │  SKIP  │      │ READ-ONLY│     │ FULL RLS │
//!                           └────────┘      └──────────┘     └──────────┘
//!                                │                │                │
//!                                ▼                ▼                ▼
//!                      ┌─────────────────────────────────────────────────────────────┐
//!                      │                     SQLite Output                           │
//!                      │                                                             │
//!                      │   (nothing)         ┌─────────────┐  ┌─────────────┐        │
//!                      │                     │ users (TBL) │  │posts_rls(TBL│        │
//!                      │                     │             │  │             │        │
//!                      │                     │  Backing    │  │  Backing    │        │
//!                      │                     │  table for  │  │  table for  │        │
//!                      │                     │  sync only  │  │  RLS data   │        │
//!                      │                     └──────┬──────┘  └──────┬──────┘        │
//!                      │                            │                │               │
//!                      │                     ┌──────▼──────┐  ┌──────▼──────┐        │
//!                      │                     │users (VIEW) │  │posts (VIEW) │        │
//!                      │                     │             │  │             │        │
//!                      │                     │ SELECT only │  │ SELECT with │        │
//!                      │                     │ (no write   │  │ RLS filter  │        │
//!                      │                     │  triggers)  │  │             │        │
//!                      │                     └─────────────┘  └──────┬──────┘        │
//!                      │                                             │               │
//!                      │                                      ┌──────▼──────┐        │
//!                      │                                      │  INSTEAD OF │        │
//!                      │                                      │  TRIGGERS   │        │
//!                      │                                      │             │        │
//!                      │                                      │ • INSERT    │        │
//!                      │                                      │ • UPDATE    │        │
//!                      │                                      │ • DELETE    │        │
//!                      │                                      │             │        │
//!                      │                                      │ (enforce    │        │
//!                      │                                      │  RLS policy)│        │
//!                      │                                      └─────────────┘        │
//!                      └─────────────────────────────────────────────────────────────┘
//! ```

mod helpers;

use diesel::prelude::*;
use helpers::{User, count_users, establish_connection, insert_user, set_session_user_id};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation},
    traits::{SessionVariableMapping, TranslationOptions},
};
use rosetta_uuid::Uuid;

/// SQL fixture with three tables demonstrating different grant patterns:
/// - `audit_logs`: No grants to app_user (server-only)
/// - `users`: SELECT only for app_user (read-only reference)
/// - `posts`: Full CRUD for app_user (user-writable with RLS)
const SQL_FIXTURE: &str = include_str!("fixtures/grant_filtering.sql");

/// Creates translation options for grant-based filtering tests.
fn translation_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string())
        .with_session_user_role("app_user")
        .with_session_variable(SessionVariableMapping::current_user("current_app_user"))
}

/// Helper to set up the database with all translated statements.
fn setup_database() -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let translated = Pg2Sqlite::default().sql(SQL_FIXTURE)?.translate(&translation_options())?;

    let mut conn = establish_connection();

    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    Ok(conn)
}

// ============================================================================
// Schema Translation Tests
// ============================================================================

/// Test that the translation correctly identifies table grant levels.
#[test]
fn test_grant_based_filtering_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let translated = Pg2Sqlite::default().sql(SQL_FIXTURE)?.translate(&translation_options())?;

    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    insta::assert_snapshot!("grant_based_filtering", translated_sql);

    Ok(())
}

/// Test that the translated SQL executes correctly in SQLite.
#[test]
fn test_grant_based_filtering_execution() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup_database()?;

    // Verify which tables exist
    let tables: Vec<TableInfo> = diesel::sql_query(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .load(&mut conn)?;

    let table_names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();

    // audit_logs should NOT exist (no grants to app_user)
    assert!(
        !table_names.contains(&"audit_logs"),
        "audit_logs should not be created (no grants to app_user)"
    );
    assert!(!table_names.contains(&"audit_logs_rls"), "audit_logs_rls should not be created");

    // users should exist as read-only (backing table for sync)
    assert!(
        table_names.contains(&"users_rls") || table_names.contains(&"users"),
        "users table should exist for read-only sync"
    );

    // posts should exist with RLS infrastructure
    assert!(table_names.contains(&"posts_rls"), "posts_rls backing table should exist");

    // Verify views exist
    let views: Vec<TableInfo> =
        diesel::sql_query("SELECT name FROM sqlite_master WHERE type='view' ORDER BY name")
            .load(&mut conn)?;

    let view_names: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();

    // users view should exist (read-only)
    assert!(view_names.contains(&"users"), "users view should exist for read-only access");

    // posts view should exist (with RLS filtering)
    assert!(view_names.contains(&"posts"), "posts view should exist with RLS");

    Ok(())
}

/// Verify that read-only tables don't have INSTEAD OF triggers for writes.
#[test]
fn test_readonly_table_no_write_triggers() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup_database()?;

    // Get all triggers
    let triggers: Vec<TriggerInfo> = diesel::sql_query(
        "SELECT name, tbl_name FROM sqlite_master WHERE type='trigger' ORDER BY name",
    )
    .load(&mut conn)?;

    // users should NOT have INSERT/UPDATE/DELETE triggers
    let users_write_triggers: Vec<_> = triggers
        .iter()
        .filter(|t| {
            t.tbl_name == "users"
                && (t.name.contains("insert")
                    || t.name.contains("update")
                    || t.name.contains("delete"))
        })
        .collect();

    assert!(
        users_write_triggers.is_empty(),
        "Read-only table 'users' should not have write triggers, found: {:?}",
        users_write_triggers.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    // posts SHOULD have write triggers
    let posts_write_triggers: Vec<_> = triggers.iter().filter(|t| t.tbl_name == "posts").collect();

    assert!(
        !posts_write_triggers.is_empty(),
        "Writable table 'posts' should have INSTEAD OF triggers"
    );

    Ok(())
}

// ============================================================================
// Helper Structs for Metadata Queries
// ============================================================================

#[derive(QueryableByName, Debug)]
struct TableInfo {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}

#[derive(QueryableByName, Debug)]
struct TriggerInfo {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    tbl_name: String,
}

// ============================================================================
// Read-Only Table Tests (users)
// ============================================================================

/// Test that SELECT from read-only table (users) works.
#[test]
fn test_readonly_select_works() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup_database()?;

    let user = User::new(Uuid::new_v4(), "testuser", "test@example.com");
    insert_user(&mut conn, &user)?;

    let count = count_users(&mut conn)?;
    assert_eq!(count, 1, "Should be able to SELECT from read-only users view");

    Ok(())
}

/// Test that INSERT into read-only table (users) fails.
#[test]
fn test_readonly_insert_fails() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup_database()?;

    let user_id = Uuid::new_v4();

    // Attempt to insert via the view (not the backing table) - should fail
    let result = diesel::sql_query(
        "INSERT INTO users (id, username, email) VALUES (?, 'testuser', 'test@example.com')",
    )
    .bind::<diesel::sql_types::Binary, _>(user_id.as_bytes().to_vec())
    .execute(&mut conn);

    assert!(result.is_err(), "INSERT into read-only 'users' view should fail, but got: {result:?}");

    Ok(())
}

/// Test that UPDATE on read-only table (users) fails.
#[test]
fn test_readonly_update_fails() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup_database()?;

    let user = User::new(Uuid::new_v4(), "testuser", "test@example.com");
    insert_user(&mut conn, &user)?;

    // Attempt to update via the view - should fail
    let result = diesel::sql_query("UPDATE users SET username = 'newname' WHERE id = ?")
        .bind::<diesel::sql_types::Binary, _>(user.id.clone())
        .execute(&mut conn);

    assert!(result.is_err(), "UPDATE on read-only 'users' view should fail, but got: {result:?}");

    Ok(())
}

/// Test that DELETE on read-only table (users) fails.
#[test]
fn test_readonly_delete_fails() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup_database()?;

    let user = User::new(Uuid::new_v4(), "testuser", "test@example.com");
    insert_user(&mut conn, &user)?;

    // Attempt to delete via the view - should fail
    let result = diesel::sql_query("DELETE FROM users WHERE id = ?")
        .bind::<diesel::sql_types::Binary, _>(user.id.clone())
        .execute(&mut conn);

    assert!(result.is_err(), "DELETE on read-only 'users' view should fail, but got: {result:?}");

    Ok(())
}

// ============================================================================
// Writable Table Tests (posts)
// ============================================================================

/// Test that INSERT into writable table (posts) succeeds with valid session
/// user.
#[test]
fn test_writable_insert_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup_database()?;

    let user_id = Uuid::new_v4();
    let user = User::new(user_id, "testuser", "test@example.com");
    insert_user(&mut conn, &user)?;

    set_session_user_id(&user_id);

    // Insert via the view - should succeed because created_by matches session user
    let post_id = Uuid::new_v4();
    let result = diesel::sql_query(
        "INSERT INTO posts (id, author_id, title, content, created_by) VALUES (?, ?, 'Test Post', 'Content', ?)",
    )
    .bind::<diesel::sql_types::Binary, _>(post_id.as_bytes().to_vec())
    .bind::<diesel::sql_types::Binary, _>(user_id.as_bytes().to_vec())
    .bind::<diesel::sql_types::Binary, _>(user_id.as_bytes().to_vec())
    .execute(&mut conn);

    assert!(
        result.is_ok(),
        "INSERT into 'posts' should succeed with valid session user, but got: {result:?}"
    );

    Ok(())
}

/// Test that INSERT into writable table (posts) fails when created_by doesn't
/// match session user.
#[test]
fn test_writable_insert_fails_wrong_user() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup_database()?;

    let user1_id = Uuid::new_v4();
    let user2_id = Uuid::new_v4();

    insert_user(&mut conn, &User::new(user1_id, "user1", "user1@example.com"))?;
    insert_user(&mut conn, &User::new(user2_id, "user2", "user2@example.com"))?;

    // Set session user to user1
    set_session_user_id(&user1_id);

    // Attempt to insert a post with created_by = user2 - should fail RLS policy
    let post_id = Uuid::new_v4();
    let result = diesel::sql_query(
        "INSERT INTO posts (id, author_id, title, content, created_by) VALUES (?, ?, 'Test Post', 'Content', ?)",
    )
    .bind::<diesel::sql_types::Binary, _>(post_id.as_bytes().to_vec())
    .bind::<diesel::sql_types::Binary, _>(user1_id.as_bytes().to_vec())
    .bind::<diesel::sql_types::Binary, _>(user2_id.as_bytes().to_vec()) // Different from session user!
    .execute(&mut conn);

    assert!(
        result.is_err(),
        "INSERT into 'posts' with wrong created_by should fail RLS policy, but got: {result:?}"
    );

    Ok(())
}
