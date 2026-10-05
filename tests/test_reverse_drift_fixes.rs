//! Pins the reverse-direction fixes for E2, E3, E5, H4, H5, H6 and I3.

use diesel::{QueryableByName, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT, n INT);";

fn schema() -> ParserDB {
    Pg2Sqlite::default().sql(SCHEMA).expect("schema").build_schema().expect("schema")
}

fn rev(sqlite_sql: &str) -> Result<String, pg2sqlite::errors::Error> {
    Pg2Sqlite::default()
        .reverse_sql(sqlite_sql, &schema(), &Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("; "))
}

fn assert_rejected_with(sqlite_sql: &str, fragment: &str) {
    let err = rev(sqlite_sql).expect_err(&format!("expected refusal for `{sqlite_sql}`"));
    assert!(
        err.to_string().contains(fragment),
        "refusal for `{sqlite_sql}` must contain {fragment:?}, got: {err}"
    );
}

fn assert_emits(sqlite_sql: &str, fragment: &str) {
    let pg =
        rev(sqlite_sql).unwrap_or_else(|e| panic!("expected success for `{sqlite_sql}`, got: {e}"));
    assert!(
        pg.contains(fragment),
        "output for `{sqlite_sql}` must contain {fragment:?}, got: {pg}"
    );
}

fn fixture_conn() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    // CREATE TABLE and INSERT have no table! schema here; sql_query is the only
    // option.
    diesel::sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT, n INTEGER)")
        .execute(&mut conn)
        .expect("ddl");
    diesel::sql_query("INSERT INTO t VALUES (1, 'hello', 42)").execute(&mut conn).expect("insert");
    conn
}

// === E2: json_extract is refused =========================================

/// `#>` returns JSONB (strings quoted, booleans as `true` not `1`); `#>>`
/// returns text for everything. Neither operator preserves all SQLite value
/// types, so the translator refuses rather than silently changing the type.
#[test]
fn e2_json_extract_is_refused() {
    // Measure the SQLite answer so the preserved value is documented.
    // sql_query: JSON function call on a literal; no typed DSL equivalent.
    #[derive(QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Text)]
        v: String,
    }
    let mut conn = fixture_conn();
    let val = diesel::sql_query("SELECT json_extract('{\"a\":\"hello\"}', '$.a') AS v")
        .get_result::<Row>(&mut conn)
        .expect("json_extract")
        .v;
    assert_eq!(val, "hello", "SQLite returns unwrapped text, not quoted JSON");

    assert_rejected_with("SELECT json_extract(s, '$.a') FROM t", "json_extract");
}

/// SQLite maps JSON `true` to integer `1`; PostgreSQL's `#>` returns the JSONB
/// boolean.
#[test]
fn e2_json_extract_boolean_becomes_1_in_sqlite() {
    // sql_query: JSON literal expression; no typed DSL equivalent.
    #[derive(QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        v: i64,
    }
    let mut conn = fixture_conn();
    let val = diesel::sql_query("SELECT json_extract('{\"ok\":true}', '$.ok') AS v")
        .get_result::<Row>(&mut conn)
        .expect("json_extract boolean")
        .v;
    assert_eq!(val, 1, "SQLite maps JSON true to integer 1");
}

// === E3: multi-argument min / max are refused =============================

/// SQLite `min(a, b)` returns NULL when any argument is NULL; PostgreSQL's
/// `LEAST` ignores NULLs. A NULL-preserving guard names each argument twice,
/// which is the operand-duplication defect this crate refuses to introduce.
#[test]
fn e3_multi_arg_min_is_refused() {
    // sql_query: bare scalar expression; no typed DSL equivalent.
    #[derive(QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
        v: Option<i64>,
    }
    let mut conn = fixture_conn();
    let val = diesel::sql_query("SELECT min(5, NULL) AS v")
        .get_result::<Row>(&mut conn)
        .expect("min(5,NULL)")
        .v;
    assert_eq!(val, None, "SQLite min(5, NULL) is NULL, not 5");

    assert_rejected_with("SELECT min(n, 0) FROM t", "NULL");
}

/// Same NULL-propagation divergence for multi-argument `max`.
#[test]
fn e3_multi_arg_max_is_refused() {
    // sql_query: bare scalar expression; no typed DSL equivalent.
    #[derive(QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
        v: Option<i64>,
    }
    let mut conn = fixture_conn();
    let val = diesel::sql_query("SELECT max(5, NULL) AS v")
        .get_result::<Row>(&mut conn)
        .expect("max(5,NULL)")
        .v;
    assert_eq!(val, None, "SQLite max(5, NULL) is NULL, not 5");

    assert_rejected_with("SELECT max(n, 0) FROM t", "NULL");
}

/// Single-argument aggregate `min` is unaffected.
#[test]
fn e3_single_arg_min_passes_through() {
    assert_emits("SELECT min(n) FROM t", "min");
}

/// Single-argument aggregate `max` is unaffected.
#[test]
fn e3_single_arg_max_passes_through() {
    assert_emits("SELECT max(n) FROM t", "max");
}

// === E5: bare LIKE passes through =========================================

/// The forward direction emits `PRAGMA case_sensitive_like = true`, aligning
/// SQLite's LIKE with PostgreSQL's; `ILIKE` would diverge on non-ASCII inputs.
/// See `test_reverse_like_contract.rs` for the full measurement.
#[test]
fn e5_plain_like_passes_through_as_like() {
    let pg = rev("SELECT s FROM t WHERE s LIKE 'hello%'").expect("LIKE reverses");
    assert!(pg.contains("LIKE") && !pg.contains("ILIKE"), "plain LIKE must not become ILIKE: {pg}");
}

/// Negated LIKE also passes through unchanged.
#[test]
fn e5_negated_plain_like_passes_through() {
    let pg = rev("SELECT s FROM t WHERE s NOT LIKE 'hello%'").expect("NOT LIKE reverses");
    assert!(pg.contains("NOT LIKE"), "NOT LIKE must pass through: {pg}");
}

// === H4: || with non-text operand is refused ==============================

/// PostgreSQL has no `text || integer`; SQLite coerces silently.
#[test]
fn h4_string_concat_integer_literal_is_refused() {
    // Measure the SQLite answer the translator is declining to reproduce.
    // sql_query: bare scalar expression; no typed DSL equivalent.
    #[derive(QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Text)]
        v: String,
    }
    let mut conn = fixture_conn();
    let val = diesel::sql_query("SELECT 'hello' || 42 AS v")
        .get_result::<Row>(&mut conn)
        .expect("SQLite concatenates text and integer")
        .v;
    assert_eq!(val, "hello42");

    assert_rejected_with("SELECT s || 42 FROM t", "text");
}

/// An integer column operand fails at the PostgreSQL server for the same
/// reason.
#[test]
fn h4_string_concat_integer_column_is_refused() {
    assert_rejected_with("SELECT s || n FROM t", "text");
}

/// `text || text` is valid PostgreSQL and passes through unchanged.
#[test]
fn h4_string_concat_text_columns_passes_through() {
    let pg = rev("SELECT s || s FROM t").expect("text || text reverses");
    assert!(pg.contains("||"), "text || text must pass through: {pg}");
}

// === H5: one-argument trunc(x) passes through =============================

/// PostgreSQL 17 answers both `trunc(numeric)` and `trunc(double precision)`,
/// measured against the server, so the one-argument form needs no rewrite.
/// The finding that claimed the overload was missing was wrong.
#[test]
fn h5_one_arg_trunc_passes_through() {
    assert_emits("SELECT trunc(n) FROM t", "trunc");
}

/// Two-argument `trunc(x, scale)` is on the shared inventory and passes
/// through.
#[test]
fn h5_two_arg_trunc_passes_through() {
    assert_emits("SELECT trunc(n, 2) FROM t", "trunc");
}

// === H6: BEGIN IMMEDIATE / DEFERRED / EXCLUSIVE → plain BEGIN =============

/// SQLite locking modifiers have no PostgreSQL equivalent; stripping them
/// gives plain `BEGIN` with the same transaction boundaries.
#[test]
fn h6_begin_immediate_becomes_plain_begin() {
    let pg = rev("BEGIN IMMEDIATE").expect("BEGIN IMMEDIATE reverses");
    assert!(pg.contains("BEGIN") && !pg.contains("IMMEDIATE"), "{pg}");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg).expect("must parse as PostgreSQL");
}

/// `BEGIN DEFERRED` strips the locking modifier.
#[test]
fn h6_begin_deferred_becomes_plain_begin() {
    let pg = rev("BEGIN DEFERRED").expect("BEGIN DEFERRED reverses");
    assert!(pg.contains("BEGIN") && !pg.contains("DEFERRED"), "{pg}");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg).expect("must parse as PostgreSQL");
}

/// `BEGIN EXCLUSIVE` strips the locking modifier.
#[test]
fn h6_begin_exclusive_becomes_plain_begin() {
    let pg = rev("BEGIN EXCLUSIVE").expect("BEGIN EXCLUSIVE reverses");
    assert!(pg.contains("BEGIN") && !pg.contains("EXCLUSIVE"), "{pg}");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg).expect("must parse as PostgreSQL");
}

/// A plain `BEGIN` passes through unchanged.
#[test]
fn h6_plain_begin_passes_through() {
    let pg = rev("BEGIN").expect("plain BEGIN reverses");
    assert!(pg.contains("BEGIN"), "{pg}");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg).expect("must parse as PostgreSQL");
}

// === I3: DML-only refusal message names transaction statements ============

/// The old message said only DML; it must now name the transaction statements
/// that also pass (`BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `RELEASE`).
#[test]
fn i3_unsupported_statement_message_names_transaction_statements() {
    let msg = rev("CREATE TABLE x (id INT)").unwrap_err().to_string();
    assert!(
        msg.contains("BEGIN")
            || msg.contains("COMMIT")
            || msg.contains("ROLLBACK")
            || msg.contains("SAVEPOINT"),
        "refusal must name at least one transaction statement: {msg}"
    );
}
