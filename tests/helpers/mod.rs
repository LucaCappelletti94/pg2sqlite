//! Shared test helpers for pg2sqlite integration tests.

#![allow(dead_code)]

#[cfg(feature = "sqlitegis")]
pub mod sqlitegis;

use std::cell::RefCell;

use diesel::{prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rosetta_uuid::Uuid;
use sqlparser::ast::Statement;

/// Translates `pg` SQL through
/// `Pg2Sqlite::default().sql(...).translate_to_sql(...)` and returns the
/// resulting SQLite statements as strings. Lets test files avoid the parse/
/// translate boilerplate; callers either unwrap, assert on the returned
/// `Result`, or join via `.join("\n")` for snapshot tests.
pub fn translate_pg(
    pg: &str,
    opts: &Pg2SqliteOptions,
) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    Pg2Sqlite::default().sql(pg)?.translate_to_sql(opts)
}

/// Returns `true` when `stmt` is a translated user-level statement of `kind`
/// (case-insensitive prefix match on `"SELECT"`, `"UPDATE"`, `"DELETE"`, etc.)
/// and not a `SELECT CreateSpatialIndex(...)` call emitted by GiST index
/// translation. The `CreateSpatialIndex` exclusion is a no-op for UPDATE and
/// DELETE since `CreateSpatialIndex` is always emitted as a `SELECT`.
#[must_use]
pub fn is_user_statement(stmt: &str, kind: &str) -> bool {
    stmt.to_ascii_uppercase().trim_start().starts_with(kind) && !stmt.contains("CreateSpatialIndex")
}

/// Finds the first translated user statement of `kind`. Panics with the full
/// statement list if none is found, which gives a useful error message in
/// tests that assert a specific DML kind was emitted.
pub fn user_statement_of<'a>(stmts: &'a [String], kind: &str) -> &'a String {
    stmts
        .iter()
        .find(|s| is_user_statement(s, kind))
        .unwrap_or_else(|| panic!("no user {kind} in:\n{}", stmts.join("\n")))
}

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

// IMPORTANT: The `*_rls` backing table schemas defined below are ONLY for
// testing. Real applications should define schemas for views, not backing
// tables, which are implementation details.

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

    uuidv7_utils::register_impl(&mut connection, Uuid::utc_v7).expect("Failed to register uuidv7");
    current_app_user_utils::register_impl(&mut connection, current_app_user_impl)
        .expect("Failed to register current_app_user");
    current_app_username_utils::register_impl(&mut connection, current_app_username_impl)
        .expect("Failed to register current_app_username");
    current_app_department_utils::register_impl(&mut connection, current_app_department_impl)
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

/// Translates PostgreSQL SQL text into SQLite AST statements.
#[allow(dead_code)]
pub fn translate_statements(
    sql: &str,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|err| err.to_string())?
        .translate(options)
        .map_err(|err| err.to_string())
}

/// Translates PostgreSQL SQL text and returns rendered SQLite SQL.
#[allow(dead_code)]
pub fn translate_sql(sql: &str, options: &Pg2SqliteOptions) -> Result<String, String> {
    translate_statements(sql, options)
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
}

/// Translates PostgreSQL SQL text and returns output statement count.
#[allow(dead_code)]
pub fn translate_count(sql: &str, options: &Pg2SqliteOptions) -> Result<usize, String> {
    translate_statements(sql, options).map(|stmts| stmts.len())
}

/// Reverse-translates SQLite SQL text back to PostgreSQL SQL.
///
/// Uses a minimal schema with table `t` for reverse translation context.
#[allow(dead_code)]
pub fn reverse_translate_sql(sql: &str) -> Result<String, String> {
    let ddl = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT, val TEXT, key TEXT, value TEXT);";
    let translator = Pg2Sqlite::default().sql(ddl).map_err(|e| e.to_string())?;
    let schema = translator.build_schema().map_err(|e| e.to_string())?;
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sql, &schema, &options).map_err(|e| e.to_string())?;
    Ok(stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
}
