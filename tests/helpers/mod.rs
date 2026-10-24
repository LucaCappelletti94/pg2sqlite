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
    Pg2Sqlite::default().sql(pg)?.translate_to_sql(opts).map(pg2sqlite::script::Script::into_vec)
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
        .map(pg2sqlite::script::Script::into_vec)
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

/// Encodes an `f32` as little-endian IEEE 754 binary16.
pub fn f32_to_f16_le(value: f32) -> [u8; 2] {
    half::f16::from_f32(value).to_le_bytes()
}

/// Opens an in-memory connection with the native vector extension.
pub fn vec_connection() -> rusqlite::Connection {
    register_sqlite_vec_once();
    rusqlite::Connection::open_in_memory().expect("in-memory SQLite")
}

// ── Shared translation helpers ─────────────────────────────────────────────

/// Translates `pg` with default options and returns the emitted statements
/// as strings.
pub fn translate_default_statements(pg: &str) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(pg)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|script| script.into_vec().iter().map(ToString::to_string).collect())
        .map_err(|e| e.to_string())
}

/// Translates `pg` with default options and returns the emitted statements
/// joined with newlines.
pub fn translate_default_joined(pg: &str) -> Result<String, String> {
    translate_default_statements(pg).map(|stmts| stmts.join("\n"))
}

/// Translates `pg` with default options and returns the emitted statements
/// joined with newlines, or the translation error message when translation
/// is refused.
pub fn translate_default_or_err(pg: &str) -> String {
    match Pg2Sqlite::default().sql(pg).unwrap().translate(&Pg2SqliteOptions::default()) {
        Err(e) => e.to_string(),
        Ok(script) => {
            script.into_vec().iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
        }
    }
}

/// Translates `{prefix}\n{pg}` with default options and returns the emitted
/// statements.
pub fn translate_prefixed(prefix: &str, pg: &str) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(&format!("{prefix}\n{pg}"))
        .map_err(|e| e.to_string())?
        .translate_to_sql(&Pg2SqliteOptions::default())
        .map(pg2sqlite::script::Script::into_vec)
        .map_err(|e| e.to_string())
}

/// Translates `pg` with default options and returns the emitted statements
/// alongside the collected translation warnings.
pub fn translate_with_default_warnings(
    pg: &str,
) -> Result<(Vec<String>, Vec<pg2sqlite::warnings::TranslationWarning>), pg2sqlite::errors::Error> {
    let report =
        Pg2Sqlite::default().sql(pg)?.translate_with_report(&Pg2SqliteOptions::default())?;
    let stmts = report.script.iter().map(ToString::to_string).collect();
    Ok((stmts, report.warnings))
}

/// Translates `pg` with `options` and applies every emitted statement to a
/// fresh in-memory connection. The emitted DDL is the artifact under test,
/// which no typed DSL can stand in for.
pub fn apply_with_options(
    pg: &str,
    options: &Pg2SqliteOptions,
) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let statements = Pg2Sqlite::default().sql(pg)?.translate(options)?.into_vec();
    let mut conn = SqliteConnection::establish(":memory:")?;
    for statement in &statements {
        // Emitted DDL is not expressible via the typed DSL.
        diesel::sql_query(statement.to_string()).execute(&mut conn)?;
    }
    Ok(conn)
}

/// Translates `pg` with default options and applies every emitted statement
/// to a fresh in-memory connection.
pub fn apply_default(pg: &str) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    apply_with_options(pg, &Pg2SqliteOptions::default())
}

/// Translates `pg` with the `rls_audit` audit table name and returns the
/// emitted statements joined with newlines.
pub fn translate_rls_sql(pg: &str) -> Result<String, Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    Ok(translate_pg(pg, &options)?.join("\n"))
}

/// Translates `pg` with the `rls_audit` audit table name and applies every
/// emitted statement to a fresh in-memory connection.
pub fn apply_rls_sql(pg: &str) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let stmts = translate_pg(pg, &options)?;
    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &stmts {
        // Emitted DDL is not expressible via the typed DSL.
        diesel::sql_query(stmt.as_str()).execute(&mut conn)?;
    }
    Ok(conn)
}

/// Translates `pg` with `opts` and applies every emitted statement to a
/// fresh in-memory connection with recursive triggers on.
pub fn apply_translated_ddl(pg: &str, opts: &Pg2SqliteOptions) -> SqliteConnection {
    let translated =
        Pg2Sqlite::default().sql(pg).expect("parse").translate(opts).expect("translate").into_vec();
    let mut conn = establish_connection();
    for statement in &translated {
        // DDL emitted by the translator cannot be expressed with the typed DSL.
        diesel::sql_query(statement.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("emitted DDL failed: {e}\n{statement}"));
    }
    conn
}

/// Runs every statement of `stmts` on a fresh in-memory connection and
/// returns it, panicking on the first failure.
pub fn apply_statements(stmts: &[String]) -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    for statement in stmts {
        // Emitted DDL is not expressible via the typed DSL.
        diesel::sql_query(statement)
            .execute(&mut conn)
            .unwrap_or_else(|error| panic!("emitted statement failed: {statement}: {error}"));
    }
    conn
}

/// Executes each non-empty newline-separated statement of `output` against
/// an in-memory SQLite, proving the joined output runs.
pub fn execute_joined_statements(output: &str) {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for s in output.split('\n').filter(|s| !s.trim().is_empty()) {
        // Translated output is a runtime string that no typed DSL can carry.
        conn.execute_batch(&format!("{s};")).unwrap();
    }
}

/// Reverse-translates `sqlite_sql` against the schema built from `pg_ddl`
/// and returns the error message. Panics when the translation succeeds.
pub fn reverse_sql_err(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    translator
        .reverse_sql(sqlite_sql, &schema, &Pg2SqliteOptions::default())
        .unwrap_err()
        .to_string()
}

/// Reverse-translates `sqlite_sql` against `schema` and returns the joined
/// statements, or the translation error.
pub fn reverse_sql_joined(
    sqlite_sql: &str,
    schema: &sql_traits::structs::ParserDB,
) -> Result<String, pg2sqlite::errors::Error> {
    Pg2Sqlite::default()
        .reverse_sql(sqlite_sql, schema, &Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
}

/// Asserts that `rev(sqlite_sql)` succeeds, the output contains `want`, and
/// the output parses as valid PostgreSQL.
pub fn assert_reverse_emits<F>(rev: F, sqlite_sql: &str, want: &str)
where
    F: Fn(&str) -> Result<String, pg2sqlite::errors::Error>,
{
    let out = rev(sqlite_sql).unwrap_or_else(|e| {
        panic!("{sqlite_sql}\n  expected output containing {want:?}, got Err: {e}")
    });
    assert!(out.contains(want), "{sqlite_sql}\n  expected {want:?} in: {out}");
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, &out)
        .unwrap_or_else(|e| {
            panic!("{sqlite_sql}\n  reverse output is not valid PostgreSQL: {e}\n{out}")
        });
}

/// Reverse-translates `sqlite_sql` against `schema`, proves the output
/// parses as PostgreSQL, and returns it.
pub fn reverse_checked(sqlite_sql: &str, schema: &sql_traits::structs::ParserDB) -> String {
    let statements = Pg2Sqlite::default()
        .reverse_sql(sqlite_sql, schema, &Pg2SqliteOptions::default())
        .expect("reverse translation");
    let sql = statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, &sql)
        .expect("output parses as PostgreSQL");
    sql
}

/// Forward-translates `pg_sql` against `schema` and joins the emitted
/// statements with `; `.
pub fn forward_translate_against(pg_sql: &str, schema: &str) -> String {
    let translator = Pg2Sqlite::default().sql(schema).unwrap();
    let built = translator.build_schema().unwrap();
    let stmts =
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, pg_sql)
            .unwrap();
    stmts
        .iter()
        .flat_map(|s| {
            pg2sqlite::prelude::Translator::translate(s, &built, &Pg2SqliteOptions::default())
                .unwrap()
        })
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Reverse-translates `sqlite_sql` against the schema built from `schema`
/// and joins the emitted statements with `; `.
pub fn reverse_translate_against(sqlite_sql: &str, schema: &str) -> String {
    let translator = Pg2Sqlite::default().sql(schema).unwrap();
    let built = translator.build_schema().unwrap();
    translator
        .reverse_sql(sqlite_sql, &built, &Pg2SqliteOptions::default())
        .unwrap()
        .into_iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Evaluates `probe` in a fresh in-memory SQLite and returns the first
/// column of the first row.
pub fn evaluate_probe<T: rusqlite::types::FromSql>(probe: &str) -> T {
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    connection
        .query_row(probe, [], |row| row.get::<_, T>(0))
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"))
}

/// Translates `pg_sql` with default options and proves every emitted
/// statement runs in SQLite.
pub fn assert_translated_statements_run(pg_sql: &str) {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for stmt in Pg2Sqlite::default()
        .sql(pg_sql)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec()
    {
        // Translated output is a runtime string that no typed DSL can carry.
        conn.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("translated statement must run in SQLite: {e}\n{stmt}"));
    }
}
