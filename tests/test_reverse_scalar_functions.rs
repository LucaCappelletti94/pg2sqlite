//! Focused tests for scalar function reverse translation.
//!
//! Checks exact emitted PostgreSQL SQL for translated cases, the error
//! message text for each rejected case, and GLOB pattern conversion.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

fn schema() -> ParserDB {
    Pg2Sqlite::default()
        .sql("CREATE TABLE t (id INT PRIMARY KEY, s TEXT, n INT, r REAL, payload JSONB);")
        .expect("fixture parses")
        .build_schema()
        .expect("fixture builds a schema")
}

fn reverse(sql: &str) -> Result<String, pg2sqlite::errors::Error> {
    let schema = schema();
    let options = Pg2SqliteOptions::default();
    let stmts = Pg2Sqlite::default().reverse_sql(sql, &schema, &options)?;
    Ok(stmts.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("; "))
}

fn ok(sql: &str) -> String {
    reverse(sql).unwrap_or_else(|e| panic!("expected Ok for `{sql}`, got Err: {e}"))
}

fn err(sql: &str) -> String {
    reverse(sql)
        .map(|out| panic!("expected Err for `{sql}`, got Ok: {out}"))
        .unwrap_err()
        .to_string()
}

// ---------- translated cases ----------

#[test]
fn ifnull_becomes_coalesce() {
    let out = ok("SELECT ifnull(n, 0) FROM t");
    assert!(out.contains("COALESCE"), "expected COALESCE in: {out}");
    assert!(out.contains("COALESCE(n, 0)"), "expected COALESCE(n, 0) in: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, &out)
        .expect("reverse output must parse as PostgreSQL");
}

#[test]
fn iif_becomes_case_when() {
    let out = ok("SELECT iif(n > 0, 1, 0) FROM t");
    assert!(out.contains("CASE WHEN"), "expected CASE WHEN in: {out}");
    assert!(out.contains("THEN"), "expected THEN in: {out}");
    assert!(out.contains("ELSE"), "expected ELSE in: {out}");
    assert!(!out.to_ascii_lowercase().contains("iif"), "iif survived into: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, &out)
        .expect("reverse output must parse as PostgreSQL");
}

#[test]
fn total_becomes_coalesce_sum() {
    let out = ok("SELECT total(n) FROM t");
    assert!(out.contains("COALESCE"), "expected COALESCE in: {out}");
    assert!(out.contains("SUM"), "expected SUM in: {out}");
    assert!(out.contains('0'), "expected 0 in: {out}");
    assert!(!out.to_ascii_lowercase().contains("total("), "total( survived into: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, &out)
        .expect("reverse output must parse as PostgreSQL");
}

#[test]
fn hex_becomes_encode_hex() {
    let out = ok("SELECT hex(s) FROM t");
    assert!(out.contains("encode"), "expected encode in: {out}");
    assert!(out.contains("'hex'"), "expected 'hex' in: {out}");
    assert!(!out.to_ascii_lowercase().contains("hex("), "hex( survived into: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, &out)
        .expect("reverse output must parse as PostgreSQL");
}

#[test]
fn unhex_becomes_decode_hex() {
    let out = ok("SELECT unhex(s) FROM t");
    assert!(out.contains("decode"), "expected decode in: {out}");
    assert!(out.contains("'hex'"), "expected 'hex' in: {out}");
    assert!(!out.to_ascii_lowercase().contains("unhex("), "unhex( survived into: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, &out)
        .expect("reverse output must parse as PostgreSQL");
}

#[test]
fn unixepoch_becomes_extract_epoch() {
    let out = ok("SELECT unixepoch(s) FROM t");
    assert!(out.contains("EXTRACT"), "expected EXTRACT in: {out}");
    assert!(out.contains("EPOCH"), "expected EPOCH in: {out}");
    assert!(!out.to_ascii_lowercase().contains("unixepoch("), "unixepoch( survived into: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, &out)
        .expect("reverse output must parse as PostgreSQL");
}

// ---------- rejected cases ----------

#[test]
fn typeof_is_rejected() {
    let msg = err("SELECT typeof(n) FROM t");
    assert!(msg.contains("typeof"), "expected 'typeof' in error: {msg}");
    assert!(
        msg.contains("pg_typeof") || msg.contains("storage-class") || msg.contains("type name"),
        "expected reason in error: {msg}"
    );
}

#[test]
fn printf_is_rejected() {
    let msg = err("SELECT printf('%d', n) FROM t");
    assert!(msg.contains("printf"), "expected 'printf' in error: {msg}");
    assert!(
        msg.contains("specifier") || msg.contains("format") || msg.contains("incompatible"),
        "expected reason in error: {msg}"
    );
}

#[test]
fn randomblob_is_rejected() {
    let msg = err("SELECT randomblob(4) FROM t");
    assert!(msg.contains("randomblob"), "expected 'randomblob' in error: {msg}");
    assert!(
        msg.contains("pgcrypto") || msg.contains("extension") || msg.contains("gen_random_bytes"),
        "expected reason in error: {msg}"
    );
}

#[test]
fn changes_is_rejected() {
    let msg = err("SELECT changes() FROM t");
    assert!(msg.contains("changes"), "expected 'changes' in error: {msg}");
    assert!(
        msg.contains("connection") || msg.contains("state") || msg.contains("no PostgreSQL"),
        "expected reason in error: {msg}"
    );
}

#[test]
fn last_insert_rowid_is_rejected() {
    let msg = err("SELECT last_insert_rowid() FROM t");
    assert!(msg.contains("last_insert_rowid"), "expected 'last_insert_rowid' in error: {msg}");
}

#[test]
fn rowid_is_rejected() {
    let msg = err("SELECT rowid FROM t");
    assert!(msg.contains("rowid"), "expected 'rowid' in error: {msg}");
}

#[test]
fn julianday_is_rejected() {
    let msg = err("SELECT julianday(s) FROM t");
    assert!(msg.contains("julianday"), "expected 'julianday' in error: {msg}");
}

#[test]
fn bare_random_is_rejected() {
    let msg = err("SELECT random() FROM t");
    assert!(msg.contains("random"), "expected 'random' in error: {msg}");
    assert!(
        msg.contains("integer")
            || msg.contains("double")
            || msg.contains("[0, 1)")
            || msg.contains("range"),
        "expected reason about value range in error: {msg}"
    );
}

// ---------- random() round-trip ----------

#[test]
fn random_round_trip_recognises_forward_emitted_pattern() {
    // The forward translator converts PostgreSQL random() to
    // (CAST(random() AS REAL) + 9223372036854775808.0) / 18446744073709551616.0
    // to normalise it to [0.0, 1.0). The reverse translator must recognise that
    // exact shape and convert it back to random().
    let forward_sql =
        "SELECT (CAST(random() AS REAL) + 9223372036854775808.0) / 18446744073709551616.0 FROM t";
    let out = ok(forward_sql);
    assert!(
        out.to_ascii_lowercase().contains("random()"),
        "expected random() in round-trip output: {out}"
    );
    Parser::parse_sql(&PostgreSqlDialect {}, &out)
        .expect("reverse output must parse as PostgreSQL");
}

// ---------- GLOB -> LIKE conversion ----------

#[test]
fn glob_star_converts_to_like_percent() {
    let out = ok("SELECT s FROM t WHERE s GLOB 'a*'");
    assert!(out.contains("LIKE 'a%'"), "expected LIKE 'a%' in: {out}");
    assert!(!out.contains("GLOB"), "GLOB survived into: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, &out)
        .expect("reverse output must parse as PostgreSQL");
}

#[test]
fn glob_question_converts_to_like_underscore() {
    let out = ok("SELECT s FROM t WHERE s GLOB 'a?b'");
    assert!(out.contains("LIKE 'a_b'"), "expected LIKE 'a_b' in: {out}");
    assert!(!out.contains("GLOB"), "GLOB survived into: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, &out)
        .expect("reverse output must parse as PostgreSQL");
}

#[test]
fn glob_literal_percent_in_pattern_is_escaped_for_like() {
    // A `%` in a GLOB pattern is literal (GLOB does not treat it as a wildcard).
    // It must be escaped in the LIKE pattern so it does not become a wildcard.
    let out = ok("SELECT s FROM t WHERE s GLOB 'a%b'");
    assert!(
        out.contains("LIKE") && out.contains(r"\%"),
        "expected escaped percent in LIKE pattern, got: {out}"
    );
    assert!(out.contains("ESCAPE"), "expected ESCAPE clause for escaped literal, got: {out}");
    assert!(!out.contains("GLOB"), "GLOB survived into: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, &out)
        .expect("reverse output must parse as PostgreSQL");
}

#[test]
fn glob_character_class_is_rejected() {
    let msg = err("SELECT s FROM t WHERE s GLOB '[abc]'");
    assert!(
        msg.contains("character class") || msg.contains('['),
        "expected character-class rejection in error: {msg}"
    );
}

#[test]
fn glob_non_literal_pattern_is_rejected() {
    let msg = err("SELECT s FROM t WHERE s GLOB s");
    assert!(
        msg.contains("non-literal") || msg.contains("literal"),
        "expected non-literal rejection in error: {msg}"
    );
}

/// The reverse entry point parses with a wrapper around `SQLiteDialect` so
/// `GLOB` becomes an operator. The wrapper must delegate every other decision
/// to the inner dialect rather than copying it: the first version reproduced
/// fourteen of the dialect's fifteen overrides and dropped
/// `supports_numeric_literal_underscores`, so `1_000` silently stopped parsing.
#[test]
fn reverse_dialect_keeps_every_sqlite_parsing_behaviour() {
    let schema = schema();
    let options = Pg2SqliteOptions::default();
    for sqlite in [
        // Numeric literal underscores, the override that was missed.
        "SELECT 1_000 FROM t",
        // A handful of other SQLite-specific parses the wrapper must not lose.
        "SELECT `s` FROM t",
        "SELECT count(*) FILTER (WHERE n > 0) FROM t",
        "SELECT n FROM t LIMIT 1, 2",
        "SELECT n FROM t WHERE n NOTNULL",
        "SELECT n FROM t WHERE n IN ()",
    ] {
        assert!(
            Pg2Sqlite::default().reverse_sql(sqlite, &schema, &options).is_ok(),
            "the wrapper must parse this exactly as SQLiteDialect does: {sqlite}"
        );
    }
}

/// `SQLiteDialect` binds the pattern operators at the caller's precedence, so a
/// following `AND` is not swallowed into the pattern.
///
/// This crate carried a wrapper dialect for this, deleted once
/// apache/datafusion-sqlparser-rs fixed the arm added in PR #2362. The test
/// stays as the reason the wrapper is not needed: if it goes red, reverse
/// translation is mis-parsing `GLOB` again.
///
/// It asserts on the tree because both parses render to the same string,
/// sqlparser's `Display` adding no parentheses.
#[test]
fn sqlite_dialect_binds_pattern_operators_at_caller_precedence() {
    fn top_level_operator(sql: &str) -> String {
        let statements =
            sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::SQLiteDialect {}, sql)
                .expect("the fixture must parse");
        let sqlparser::ast::Statement::Query(query) = &statements[0] else {
            panic!("expected a query");
        };
        let sqlparser::ast::SetExpr::Select(select) = &*query.body else {
            panic!("expected a select");
        };
        match select.selection.as_ref().expect("the fixture has a WHERE") {
            sqlparser::ast::Expr::BinaryOp { op, .. } => op.to_string(),
            other => panic!("expected a binary operator at the top, got {other:?}"),
        }
    }

    for operator in ["LIKE", "GLOB", "REGEXP", "MATCH"] {
        let sql = format!("SELECT s FROM t WHERE s {operator} 'p' AND n = 1");
        assert_eq!(
            top_level_operator(&sql),
            "AND",
            "{operator} must not swallow the AND into its pattern"
        );
    }
}
