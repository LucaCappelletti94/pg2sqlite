//! Shared test helpers for pg2sqlite integration tests.

#![allow(dead_code)]

use std::cell::RefCell;

use diesel::{prelude::*, sqlite::SqliteConnection};
use rosetta_uuid::Uuid;

#[declare_sql_function]
extern "SQL" {
    /// Generates a UUIDv7 value as a BLOB.
    fn uuidv7() -> diesel::sql_types::Binary;
    /// Returns the current application user ID.
    fn current_app_user() -> diesel::sql_types::Binary;
    /// Returns the current application username (for current_user mapping).
    fn current_app_username() -> diesel::sql_types::Text;
    /// Returns the current user's department (for department-based RLS).
    fn current_app_department() -> diesel::sql_types::Text;
}

// ============================================================================
// Diesel Schema Definitions for RLS backing tables
// ============================================================================
//
// IMPORTANT: The `*_rls` backing table schemas defined below are ONLY for
// testing the RLS translation implementation. Real applications should NOT
// define schemas for backing tables - they are implementation details of the
// RLS translation.
//
// In production code, only define schemas for the VIEWS (e.g., `users`,
// `posts`), not the backing tables (e.g., `users_rls`, `posts_rls`). The view
// schemas provide transparent RLS enforcement through INSTEAD OF triggers.
//
// These backing table schemas are included here to:
// - Simulate direct server-side data insertion (bypassing RLS)
// - Verify that triggers correctly synchronize data between views and backing
//   tables
// - Test the internal mechanics of the RLS translation
// ============================================================================

diesel::table! {
    /// The backing table for users (read-only via view).
    ///
    /// **Testing only** - Real applications should use the `users` view schema.
    users_rls (id) {
        id -> Binary,
        username -> Text,
        email -> Text,
    }
}

diesel::table! {
    /// The backing table for posts (writable via view with RLS).
    ///
    /// **Testing only** - Real applications should use the `posts` view schema.
    posts_rls (id) {
        id -> Binary,
        author_id -> Binary,
        title -> Text,
        content -> Nullable<Text>,
        created_by -> Binary,
    }
}

diesel::table! {
    /// The users view (RLS-filtered).
    ///
    /// **This is what real applications should use** - the view provides transparent
    /// RLS enforcement. All queries and mutations go through this view.
    users (id) {
        id -> Binary,
        username -> Text,
        email -> Text,
    }
}

diesel::table! {
    /// The posts view (RLS-filtered).
    ///
    /// **This is what real applications should use** - the view provides transparent
    /// RLS enforcement. INSTEAD OF triggers handle INSERT/UPDATE/DELETE operations.
    posts (id) {
        id -> Binary,
        author_id -> Binary,
        title -> Text,
        content -> Nullable<Text>,
        created_by -> Binary,
    }
}

diesel::joinable!(posts_rls -> users_rls (author_id));
diesel::joinable!(posts -> users (author_id));
diesel::allow_tables_to_appear_in_same_query!(users_rls, posts_rls, users, posts);

// ============================================================================
// Model Structs
// ============================================================================

/// A user in the system.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, PartialEq, Eq)]
#[diesel(table_name = users_rls)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct User {
    /// The user's unique identifier.
    pub id: Vec<u8>,
    /// The user's username.
    pub username: String,
    /// The user's email address.
    pub email: String,
}

impl User {
    /// Creates a new user with the given details.
    #[must_use]
    pub fn new(id: Uuid, username: impl Into<String>, email: impl Into<String>) -> Self {
        Self { id: id.as_bytes().to_vec(), username: username.into(), email: email.into() }
    }

    /// Returns the user's ID as a UUID.
    #[must_use]
    pub fn uuid(&self) -> Uuid {
        let bytes: [u8; 16] = self.id.clone().try_into().expect("Invalid UUID length");
        Uuid::from(bytes)
    }
}

/// A post created by a user.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, PartialEq, Eq)]
#[diesel(table_name = posts_rls)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Post {
    /// The post's unique identifier.
    pub id: Vec<u8>,
    /// The author's user ID.
    pub author_id: Vec<u8>,
    /// The post title.
    pub title: String,
    /// The post content (optional).
    pub content: Option<String>,
    /// The user who created this post (for RLS).
    pub created_by: Vec<u8>,
}

impl Post {
    /// Creates a new post with the given details.
    #[must_use]
    pub fn new(
        id: Uuid,
        author_id: Uuid,
        title: impl Into<String>,
        content: Option<String>,
        created_by: Uuid,
    ) -> Self {
        Self {
            id: id.as_bytes().to_vec(),
            author_id: author_id.as_bytes().to_vec(),
            title: title.into(),
            content,
            created_by: created_by.as_bytes().to_vec(),
        }
    }

    /// Returns the post's ID as a UUID.
    #[must_use]
    pub fn uuid(&self) -> Uuid {
        let bytes: [u8; 16] = self.id.clone().try_into().expect("Invalid UUID length");
        Uuid::from(bytes)
    }
}

// ============================================================================
// Session User Management
// ============================================================================

// Thread-local storage for the current session user ID
thread_local! {
    static SESSION_USER_ID: RefCell<Option<rosetta_uuid::Uuid>> = const { RefCell::new(None) };
    static SESSION_USERNAME: RefCell<Option<String>> = const { RefCell::new(None) };
    static SESSION_DEPARTMENT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Sets the current session user ID for RLS filtering.
#[allow(dead_code)]
pub fn set_session_user_id(user_id: &Uuid) {
    SESSION_USER_ID.with(|u| {
        *u.borrow_mut() = Some(*user_id);
    });
}

/// Clears the current session user ID.
#[allow(dead_code)]
fn clear_session_user_id() {
    SESSION_USER_ID.with(|u| {
        *u.borrow_mut() = None;
    });
}

/// Sets the current session username for RLS filtering (for current_user
/// mapping).
#[allow(dead_code)]
pub fn set_session_username(username: &str) {
    SESSION_USERNAME.with(|u| {
        *u.borrow_mut() = Some(username.to_string());
    });
}

/// Sets the current session department for RLS filtering.
#[allow(dead_code)]
pub fn set_session_department(department: Option<&str>) {
    SESSION_DEPARTMENT.with(|d| {
        *d.borrow_mut() = department.map(ToString::to_string);
    });
}

/// Implementation of the current_app_username function for SQLite.
/// Returns the current username as text, or panics if not set.
fn current_app_username_impl() -> String {
    SESSION_USERNAME.with(|u| {
        (*u.borrow()).clone().expect("Session username not set - call set_session_username() first")
    })
}

/// Implementation of the current_app_department function for SQLite.
/// Returns the current department as text, or empty string if not set (mimics
/// PostgreSQL's current_setting with missing_ok=true).
fn current_app_department_impl() -> String {
    SESSION_DEPARTMENT.with(|d| (*d.borrow()).clone().unwrap_or_default())
}

/// Implementation of the current_app_user function for SQLite.
/// Returns the current user ID as a blob, or panics if not set.
fn current_app_user_impl() -> rosetta_uuid::Uuid {
    SESSION_USER_ID.with(|u| {
        (*u.borrow()).expect("Session user ID not set - call set_session_user_id() first")
    })
}

/// Establishes an in-memory SQLite connection with:
/// - Foreign keys enabled
/// - Recursive triggers enabled
///
/// Note: The caller must register the uuidv7 function after calling this,
/// using the `uuidv7_utils` module generated by `#[declare_sql_function]`.
pub fn establish_connection() -> SqliteConnection {
    let mut connection =
        SqliteConnection::establish(":memory:").expect("Error connecting to in-memory SQLite");

    diesel::sql_query("PRAGMA foreign_keys = ON")
        .execute(&mut connection)
        .expect("Failed to enable foreign key constraints");

    diesel::sql_query("PRAGMA recursive_triggers = ON")
        .execute(&mut connection)
        .expect("Failed to enable recursive triggers");

    uuidv7_utils::register_impl(&connection, Uuid::utc_v7).expect("Failed to register uuidv7");
    current_app_user_utils::register_impl(&connection, current_app_user_impl)
        .expect("Failed to register current_app_user");
    current_app_username_utils::register_impl(&connection, current_app_username_impl)
        .expect("Failed to register current_app_username");
    current_app_department_utils::register_impl(&connection, current_app_department_impl)
        .expect("Failed to register current_app_department");

    connection
}

#[allow(dead_code)]
/// Common query result type for counting rows.
#[derive(QueryableByName, Debug)]
pub struct Count {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub count: i64,
}

/// Common query result type for UUID id fields.
#[allow(dead_code)]
#[derive(QueryableByName, Debug)]
pub struct IdResult {
    #[diesel(sql_type = diesel::sql_types::Binary)]
    pub id: Vec<u8>,
}

// ============================================================================
// Database Operations
// ============================================================================

/// Inserts a user into the backing table (simulating sync from server).
///
/// **Testing only** - This directly inserts into the backing table, bypassing
/// RLS. Real applications should insert into the `users` view, which enforces
/// RLS policies.
#[allow(dead_code)]
pub fn insert_user(conn: &mut SqliteConnection, user: &User) -> QueryResult<usize> {
    diesel::insert_into(users_rls::table).values(user).execute(conn)
}

/// Inserts a post into the backing table (simulating sync from server).
///
/// **Testing only** - This directly inserts into the backing table, bypassing
/// RLS. Real applications should insert into the `posts` view, which enforces
/// RLS policies.
#[allow(dead_code)]
pub fn insert_post_rls(conn: &mut SqliteConnection, post: &Post) -> QueryResult<usize> {
    diesel::insert_into(posts_rls::table).values(post).execute(conn)
}

/// Counts rows in the users view.
#[allow(dead_code)]
pub fn count_users(conn: &mut SqliteConnection) -> QueryResult<i64> {
    users::table.count().get_result(conn)
}

/// Counts rows in the posts view.
#[allow(dead_code)]
pub fn count_posts(conn: &mut SqliteConnection) -> QueryResult<i64> {
    posts::table.count().get_result(conn)
}
