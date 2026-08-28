//! Test for grant-based table filtering in SQLite generation.
//!
//! Tables without grants are omitted, SELECT-only tables get a read-only view
//! with exemptible backing guards, and writable tables get full RLS triggers.
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

use std::cell::Cell;

mod helpers;

use diesel::prelude::*;
use helpers::{establish_connection, set_session_user_id};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation},
    traits::SessionVariableMapping,
};
use rosetta_uuid::Uuid;

diesel::define_sql_function! {
    /// Reports whether generated write guards are exempt.
    fn write_is_exempt() -> diesel::sql_types::Bool;
}

thread_local! {
    static WRITE_IS_EXEMPT: Cell<bool> = const { Cell::new(false) };
}

diesel::table! {
    /// Backing table for users with RLS (read-only for app_user).
    users_rls (id) {
        /// User ID (UUIDv7 as BLOB).
        id -> Binary,
        /// Username.
        username -> Text,
        /// Email address.
        email -> Text,
    }
}

diesel::table! {
    /// View for users (read-only access).
    users (id) {
        /// User ID (UUIDv7 as BLOB).
        id -> Binary,
        /// Username.
        username -> Text,
        /// Email address.
        email -> Text,
    }
}

diesel::table! {
    /// Backing table for posts with RLS (writable with policies).
    posts_rls (id) {
        /// Post ID (UUIDv7 as BLOB).
        id -> Binary,
        /// Author user ID.
        author_id -> Binary,
        /// Post title.
        title -> Text,
        /// Post content.
        content -> Nullable<Text>,
        /// User who created this post (for RLS).
        created_by -> Binary,
    }
}

diesel::table! {
    /// View for posts (RLS-filtered with INSTEAD OF triggers).
    posts (id) {
        /// Post ID (UUIDv7 as BLOB).
        id -> Binary,
        /// Author user ID.
        author_id -> Binary,
        /// Post title.
        title -> Text,
        /// Post content.
        content -> Nullable<Text>,
        /// User who created this post (for RLS).
        created_by -> Binary,
    }
}

diesel::joinable!(posts_rls -> users_rls (author_id));
diesel::joinable!(posts -> users (author_id));
diesel::allow_tables_to_appear_in_same_query!(users_rls, posts_rls, users, posts);

/// A user in the system (for backing table).
#[derive(Debug, Clone, Queryable, Selectable, Insertable, PartialEq, Eq)]
#[diesel(table_name = users_rls)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct User {
    /// User ID (UUIDv7 as BLOB).
    id: Vec<u8>,
    /// Username.
    username: String,
    /// Email address.
    email: String,
}

impl User {
    /// Creates a new user with the given details.
    fn new(id: Uuid, username: impl Into<String>, email: impl Into<String>) -> Self {
        Self { id: id.as_bytes().to_vec(), username: username.into(), email: email.into() }
    }
}

/// A user for insertion into the users view (read-only, should fail).
#[derive(Debug, Clone, Insertable, PartialEq, Eq)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct UserView {
    /// User ID (UUIDv7 as BLOB).
    id: Vec<u8>,
    /// Username.
    username: String,
    /// Email address.
    email: String,
}

/// A post created by a user (for backing table).
#[derive(Debug, Clone, Queryable, Selectable, Insertable, PartialEq, Eq)]
#[diesel(table_name = posts_rls)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Post {
    /// Post ID (UUIDv7 as BLOB).
    id: Vec<u8>,
    /// Author user ID.
    author_id: Vec<u8>,
    /// Post title.
    title: String,
    /// Post content.
    content: Option<String>,
    /// User who created this post (for RLS).
    created_by: Vec<u8>,
}

/// A post for insertion into the posts view (with INSTEAD OF triggers).
#[derive(Debug, Clone, Insertable, PartialEq, Eq)]
#[diesel(table_name = posts)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct PostView {
    /// Post ID (UUIDv7 as BLOB).
    id: Vec<u8>,
    /// Author user ID.
    author_id: Vec<u8>,
    /// Post title.
    title: String,
    /// Post content.
    content: Option<String>,
    /// User who created this post (for RLS).
    created_by: Vec<u8>,
}

/// SQL fixture with three tables demonstrating different grant patterns:
/// - `audit_logs`: No grants to app_user (server-only)
/// - `users`: SELECT only for app_user (read-only reference)
/// - `posts`: Full CRUD for app_user (user-writable with RLS)
const SQL_FIXTURE: &str = include_str!("fixtures/grant_filtering.sql");

/// Creates translation options for grant-based filtering tests.
fn translation_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_session_user_role("app_user")
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_user("current_app_user"))
        .with_write_exemption_function("write_is_exempt")
}

/// Helper to set up the database with all translated statements.
fn setup_database() -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let translated = Pg2Sqlite::default().sql(SQL_FIXTURE)?.translate(&translation_options())?;

    let mut conn = establish_connection();
    write_is_exempt_utils::register_nondeterministic_impl(&conn, || {
        WRITE_IS_EXEMPT.with(Cell::get)
    })?;

    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    Ok(conn)
}

fn insert_synced_user(
    conn: &mut SqliteConnection,
    user: &User,
) -> Result<usize, diesel::result::Error> {
    WRITE_IS_EXEMPT.with(|exempt| exempt.set(true));
    let result = diesel::insert_into(users_rls::table).values(user).execute(conn);
    WRITE_IS_EXEMPT.with(|exempt| exempt.set(false));
    result
}

#[test]
fn test_grant_based_filtering_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let translated = Pg2Sqlite::default().sql(SQL_FIXTURE)?.translate(&translation_options())?;

    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    insta::assert_snapshot!("grant_based_filtering", translated_sql);

    Ok(())
}

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

/// A read-only view has no INSTEAD OF write triggers.
#[test]
fn test_readonly_view_has_no_write_triggers() -> Result<(), Box<dyn std::error::Error>> {
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

#[test]
fn test_readonly_select_works() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup_database()?;

    let user = User::new(Uuid::new_v4(), "testuser", "test@example.com");
    insert_synced_user(&mut conn, &user)?;

    let count: i64 = users::table.count().get_result(&mut conn)?;
    assert_eq!(count, 1, "Should be able to SELECT from read-only users view");

    Ok(())
}

#[test]
fn test_readonly_insert_fails() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup_database()?;

    let user_id = Uuid::new_v4();
    let user_view = UserView {
        id: user_id.as_bytes().to_vec(),
        username: "testuser".to_string(),
        email: "test@example.com".to_string(),
    };

    // Attempt to insert via the view (not the backing table) - should fail
    let result = diesel::insert_into(users::table).values(&user_view).execute(&mut conn);

    assert!(result.is_err(), "INSERT into read-only 'users' view should fail, but got: {result:?}");

    Ok(())
}

#[test]
fn test_readonly_update_fails() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup_database()?;

    let user = User::new(Uuid::new_v4(), "testuser", "test@example.com");
    insert_synced_user(&mut conn, &user)?;

    // Attempt to update via the view - should fail
    let result = diesel::update(users::table.filter(users::id.eq(&user.id)))
        .set(users::username.eq("newname"))
        .execute(&mut conn);

    assert!(result.is_err(), "UPDATE on read-only 'users' view should fail, but got: {result:?}");

    Ok(())
}

#[test]
fn test_readonly_delete_fails() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup_database()?;

    let user = User::new(Uuid::new_v4(), "testuser", "test@example.com");
    insert_synced_user(&mut conn, &user)?;

    // Attempt to delete via the view - should fail
    let result = diesel::delete(users::table.filter(users::id.eq(&user.id))).execute(&mut conn);

    assert!(result.is_err(), "DELETE on read-only 'users' view should fail, but got: {result:?}");

    Ok(())
}

/// Test that INSERT into writable table (posts) succeeds with valid session
/// user.
#[test]
fn test_writable_insert_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup_database()?;

    let user_id = Uuid::new_v4();
    let user = User::new(user_id, "testuser", "test@example.com");
    insert_synced_user(&mut conn, &user)?;

    set_session_user_id(&user_id);

    // Insert via the view - should succeed because created_by matches session user
    let post = PostView {
        id: Uuid::new_v4().as_bytes().to_vec(),
        author_id: user_id.as_bytes().to_vec(),
        title: "Test Post".to_string(),
        content: Some("Content".to_string()),
        created_by: user_id.as_bytes().to_vec(),
    };

    let result = diesel::insert_into(posts::table).values(&post).execute(&mut conn);

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

    insert_synced_user(&mut conn, &User::new(user1_id, "user1", "user1@example.com"))?;
    insert_synced_user(&mut conn, &User::new(user2_id, "user2", "user2@example.com"))?;

    // Set session user to user1
    set_session_user_id(&user1_id);

    // Attempt to insert a post with created_by = user2 - should fail RLS policy
    let post = PostView {
        id: Uuid::new_v4().as_bytes().to_vec(),
        author_id: user1_id.as_bytes().to_vec(),
        title: "Test Post".to_string(),
        content: Some("Content".to_string()),
        created_by: user2_id.as_bytes().to_vec(), // Different from session user!
    };

    let result = diesel::insert_into(posts::table).values(&post).execute(&mut conn);

    assert!(
        result.is_err(),
        "INSERT into 'posts' with wrong created_by should fail RLS policy, but got: {result:?}"
    );

    Ok(())
}
