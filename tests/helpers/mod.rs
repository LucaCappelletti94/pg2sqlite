//! Shared test helpers for pg2sqlite integration tests.

#![allow(dead_code)]

#[cfg(feature = "sqlitegis")]
pub mod sqlitegis;

use std::{cell::RefCell, sync::Once};

use diesel::{prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rosetta_uuid::Uuid;
use sqlite_vec::sqlite3_vec_init;
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

/// Translates `pg` and executes every emitted statement in a fresh in-memory
/// SQLite.
///
/// The proof an emitted script is valid SQLite: the panic names the statement
/// that was rejected and the error SQLite gave for it.
///
/// # Panics
///
/// Panics when translation fails or when an emitted statement will not
/// execute.
pub fn execute_all(pg: &str, options: &Pg2SqliteOptions) {
    let statements = translate_pg(pg, options).expect("translation should succeed");
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection.execute_batch(&format!("{statement};")).unwrap_or_else(|error| {
            panic!("emitted statement must execute in SQLite: {error}\n{statement}")
        });
    }
}

/// Applies everything `pg` emits except the query under test, then returns
/// that query once SQLite has accepted it.
///
/// The query is picked with [`user_statement_of`], so a `SELECT
/// CreateSpatialIndex(...)` emitted by a GiST index is applied as setup
/// rather than mistaken for the query the test asserts on.
///
/// # Panics
///
/// Panics when translation fails, when no user `SELECT` is emitted, or when
/// SQLite rejects any emitted statement.
pub fn prepared_user_select(pg: &str, options: &Pg2SqliteOptions) -> String {
    let statements = translate_pg(pg, options).expect("translation should succeed");
    let select = statements
        .iter()
        .position(|statement| is_user_statement(statement, "SELECT"))
        .unwrap_or_else(|| panic!("no user SELECT in:\n{}", statements.join("\n")));

    let connection = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    for (index, statement) in statements.iter().enumerate() {
        if index == select {
            continue;
        }
        connection.execute_batch(&format!("{statement};")).unwrap_or_else(|error| {
            panic!("emitted statement must execute in SQLite: {error}\n{statement}")
        });
    }
    connection.prepare(&statements[select]).unwrap_or_else(|error| {
        panic!("SQLite must accept the query: {error}\n{}", statements[select])
    });

    statements[select].clone()
}

/// Registers sqlite-vec once per process, so every later connection has it.
pub fn register_sqlite_vec_once() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // SAFETY: `sqlite3_vec_init` is sqlite-vec's C entry point, whose
        // signature is `(db, pzErrMsg, pApi) -> int`; the transmute restores
        // that type so the C API can store and call it.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut std::os::raw::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> i32,
            >(
                sqlite3_vec_init as *const ()
            )));
        }
    });
}

/// One `f32` as the 2 bytes of a little-endian IEEE 754 half.
#[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn f32_to_f16_le(x: f32) -> [u8; 2] {
    let b: u32 = x.to_bits();
    let sign: u16 = {
        debug_assert!(b >> 31 <= 1);
        (b >> 31) as u16 // deliberate: single-bit extraction, value 0 or 1
    } << 15;
    let exp32: i32 = {
        let e = (b >> 23) & 0xFF;
        debug_assert!(e <= 255);
        e as i32 // deliberate: 8-bit field, always 0..=255, fits i32
    };
    let mantissa: u32 = b & 0x7F_FFFF;
    let bits: u16 = if exp32 == 0xFF {
        let top10: u16 = {
            let m = mantissa >> 13;
            debug_assert!(m <= 0x3FF);
            m as u16 // deliberate: top-10 mantissa bits, value <= 0x3FF
        };
        if mantissa != 0 { 0x7E00 | sign | top10 } else { 0x7C00 | sign }
    } else if exp32 == 0 {
        sign
    } else {
        let e = exp32 - 127 + 15;
        if e >= 31 {
            0x7C00 | sign
        } else if e <= 0 {
            sign
        } else {
            debug_assert!(e > 0 && e <= 30);
            debug_assert!(mantissa >> 13 <= 0x3FF);
            let e16: u16 = e as u16; // deliberate: proven 1..=30, fits u16
            let m16: u16 = (mantissa >> 13) as u16; // deliberate: <= 0x3FF, fits u16
            sign | (e16 << 10) | m16
        }
    };
    bits.to_le_bytes()
}

/// Registers `vec_f16` on `conn`, which sqlite-vec 0.1.9 does not ship.
///
/// Produces true 2-byte halves, which is what a `::halfvec` cast means. A
/// test that inserts into a `vec0` table needs its own float32-encoding shim
/// instead, because vec0 0.1.9 stores `float16[]` as float32 and rejects a
/// blob whose length is not divisible by four.
///
/// rusqlite directly, because diesel exposes neither `sqlite3_auto_extension`
/// nor `create_scalar_function`.
pub fn register_vec_f16(conn: &rusqlite::Connection) {
    conn.create_scalar_function(
        "vec_f16",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8
            | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            match ctx.get_raw(0) {
                rusqlite::types::ValueRef::Null => Ok(rusqlite::types::Value::Null),
                rusqlite::types::ValueRef::Text(t) => {
                    let text = String::from_utf8_lossy(t);
                    let trimmed = text.trim().trim_start_matches('[').trim_end_matches(']');
                    let bytes: Vec<u8> = trimmed
                        .split(',')
                        .filter_map(|s| s.trim().parse::<f32>().ok())
                        .flat_map(f32_to_f16_le)
                        .collect();
                    Ok(rusqlite::types::Value::Blob(bytes))
                }
                _ => {
                    Err(rusqlite::Error::InvalidFunctionParameterType(
                        0,
                        rusqlite::types::Type::Text,
                    ))
                }
            }
        },
    )
    .expect("register vec_f16");
}

/// A fresh in-memory connection with sqlite-vec loaded and `vec_f16` present.
pub fn vec_connection() -> rusqlite::Connection {
    register_sqlite_vec_once();
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    register_vec_f16(&conn);
    conn
}
