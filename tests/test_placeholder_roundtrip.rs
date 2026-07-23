//! Placeholder translation must round trip between the dialects without losing
//! the bind index. The number a placeholder carries is what a bind vector keys
//! on, so translating SQLite to PostgreSQL and back (or the reverse) must
//! recover the same indices. The canonical numbered forms `?N` and `$N` map to
//! each other byte for byte; a bare `?` canonicalizes to `?N` on its first loop
//! (the index SQLite itself would assign it) and then stays fixed.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, Translator};
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

const SCHEMA: &str = "CREATE TABLE t (a INT, b INT, c INT, d INT);";

/// SQLite SQL text to PostgreSQL SQL text.
fn reverse(sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(SCHEMA).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();
    stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
}

/// PostgreSQL SQL text to SQLite SQL text.
fn forward(pg_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(SCHEMA).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = Parser::parse_sql(&PostgreSqlDialect {}, pg_sql).unwrap();
    stmts
        .iter()
        .flat_map(|s| s.translate(&schema, &options).unwrap())
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

#[test]
fn numbered_sqlite_survives_a_full_loop() {
    let original = "SELECT * FROM t WHERE a = ?2 AND b = ?1";
    let postgres = reverse(original);
    assert_eq!(postgres, "SELECT * FROM t WHERE a = $2 AND b = $1");
    assert_eq!(forward(&postgres), original);
}

#[test]
fn numbered_postgres_survives_a_full_loop() {
    let original = "SELECT * FROM t WHERE a = $1 AND b = $2";
    let sqlite = forward(original);
    assert_eq!(sqlite, "SELECT * FROM t WHERE a = ?1 AND b = ?2");
    assert_eq!(reverse(&sqlite), original);
}

#[test]
fn bare_positional_canonicalizes_then_reaches_a_fixed_point() {
    let original = "SELECT * FROM t WHERE a = ? AND b = ?";

    // First loop canonicalizes bare `?` to explicit `?N`, preserving the index
    // SQLite would have assigned (1, then 2).
    let postgres_1 = reverse(original);
    assert_eq!(postgres_1, "SELECT * FROM t WHERE a = $1 AND b = $2");
    let sqlite_1 = forward(&postgres_1);
    assert_eq!(sqlite_1, "SELECT * FROM t WHERE a = ?1 AND b = ?2");

    // Second loop is the identity: the representation is now a fixed point.
    let postgres_2 = reverse(&sqlite_1);
    assert_eq!(postgres_2, postgres_1);
    let sqlite_2 = forward(&postgres_2);
    assert_eq!(sqlite_2, sqlite_1);
}

#[test]
fn mixed_assignment_rule_is_stable_after_first_loop() {
    // The SQLite bare-`?` rule assigns 1, 5, 6. After one loop the SQL is fully
    // explicit and every later loop preserves those exact indices.
    let original = "SELECT * FROM t WHERE a = ? AND b = ?5 AND c = ?";
    let postgres_1 = reverse(original);
    assert_eq!(postgres_1, "SELECT * FROM t WHERE a = $1 AND b = $5 AND c = $6");

    let sqlite_1 = forward(&postgres_1);
    assert_eq!(sqlite_1, "SELECT * FROM t WHERE a = ?1 AND b = ?5 AND c = ?6");

    // Re-entering reverse recovers the identical PostgreSQL parameters.
    assert_eq!(reverse(&sqlite_1), postgres_1);
}

#[test]
fn insert_parameters_survive_a_full_loop() {
    let original = "INSERT INTO t (a, b) VALUES (?1, ?2)";
    let postgres = reverse(original);
    assert_eq!(postgres, "INSERT INTO t (a, b) VALUES ($1, $2)");
    assert_eq!(forward(&postgres), original);
}
