//! Reverse-translation fixes: `#>` / `#>>` path conversion, `unixepoch(x)`
//! fraction truncation, and `json_type(x)` vocabulary refusal.
//!
//! All assertions are on the emitted SQL text and whether a call is accepted
//! or refused; no database execution is needed to prove any of the three
//! changes.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP, payload JSONB);";

fn schema() -> ParserDB {
    Pg2Sqlite::default().sql(SCHEMA).expect("schema parse").build_schema().expect("schema build")
}

fn rev(sqlite_sql: &str) -> Result<String, pg2sqlite::errors::Error> {
    let schema = schema();
    let options = Pg2SqliteOptions::default();
    Pg2Sqlite::default()
        .reverse_sql(sqlite_sql, &schema, &options)
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
}

fn assert_emits(sqlite_sql: &str, want: &str) {
    let out = rev(sqlite_sql).unwrap_or_else(|e| {
        panic!("{sqlite_sql}\n  expected output containing {want:?}, got Err: {e}")
    });
    assert!(out.contains(want), "{sqlite_sql}\n  expected {want:?} in: {out}");
    Parser::parse_sql(&PostgreSqlDialect {}, &out).unwrap_or_else(|e| {
        panic!("{sqlite_sql}\n  reverse output is not valid PostgreSQL: {e}\n{out}")
    });
}

fn assert_rejected_with(sqlite_sql: &str, want: &str) {
    match rev(sqlite_sql) {
        Ok(out) => panic!("{sqlite_sql}\n  expected Err containing {want:?}, got: {out}"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains(want),
                "{sqlite_sql}\n  expected error containing {want:?}, got: {msg}"
            );
        }
    }
}

// === #> and #>> operator: SQLite JSONPath -> PostgreSQL text-array path ======

/// A single-key JSONPath right operand is converted to a text-array path.
#[test]
fn hash_arrow_converts_sqlite_json_path() {
    assert_emits("SELECT payload #> '$.a' FROM t", "#> '{a}'");
}

/// Same for the text form `#>>`.
#[test]
fn hash_long_arrow_converts_sqlite_json_path() {
    assert_emits("SELECT payload #>> '$.a' FROM t", "#>> '{a}'");
}

/// A dotted path expands to a multi-key text array.
#[test]
fn hash_arrow_nested_path_expands_to_multi_key() {
    assert_emits("SELECT payload #> '$.a.b' FROM t", "#> '{a,b}'");
}

/// A non-literal right operand cannot be read at translation time and is
/// refused rather than passed through.
#[test]
fn hash_arrow_non_literal_right_operand_is_refused() {
    assert_rejected_with("SELECT payload #> id FROM t", "string literal");
}

/// A path containing an array index has no text-array equivalent and is
/// refused.
#[test]
fn hash_arrow_array_index_path_is_refused() {
    assert_rejected_with("SELECT payload #> '$.a[0]' FROM t", "cannot be converted");
}

/// Round-trip: the forward direction emits `->` chains for `#>`, and those
/// chains pass through the reverse translator as valid PostgreSQL.
#[test]
fn hash_arrow_round_trip_forward_output_is_valid_postgres() {
    let sql = format!("{SCHEMA}\nSELECT payload #> '{{a}}' FROM t;");
    let tr = Pg2Sqlite::default().sql(&sql).expect("forward parse");
    let stmts = tr.clone().translate(&Pg2SqliteOptions::default()).expect("forward translate");
    let forward_sqlite = stmts
        .iter()
        .map(ToString::to_string)
        .find(|s| s.contains("SELECT"))
        .expect("a SELECT statement");
    // The forward direction lowers #> onto arrow chains, not #> itself.
    assert!(!forward_sqlite.contains("#>"), "forward emits -> chains: {forward_sqlite}");
    // The reverse direction must accept the forward output and emit valid PG.
    let reversed = tr
        .reverse_sql(&format!("{forward_sqlite};"), &schema(), &Pg2SqliteOptions::default())
        .expect("round-trip reverse");
    let pg = reversed.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg)
        .unwrap_or_else(|e| panic!("round-trip output not valid PostgreSQL: {e}\n{pg}"));
}

// === unixepoch(x) one-argument form: whole seconds ==========================

/// SQLite's one-argument `unixepoch(x)` answers whole seconds, exactly like
/// the zero-argument form. The same `floor(...)::BIGINT` wrap applies.
#[test]
fn unixepoch_one_arg_emits_floor_and_bigint_cast() {
    let pg = rev("SELECT unixepoch(ts) FROM t").expect("one-arg unixepoch reverses");
    assert!(pg.contains("floor"), "the fraction must be floored: {pg}");
    assert!(pg.contains("EPOCH"), "must use EXTRACT EPOCH: {pg}");
    assert!(pg.contains("BIGINT"), "SQLite answers an integer: {pg}");
}

/// The `subsec` modifier keeps the fraction; the floor must not appear.
#[test]
fn unixepoch_subsec_form_stays_unfloored() {
    let pg = rev("SELECT unixepoch(ts, 'subsec') FROM t").expect("subsec unixepoch reverses");
    assert!(pg.contains("EPOCH"), "{pg}");
    assert!(!pg.contains("floor"), "subsec is faithful, no floor: {pg}");
    assert!(!pg.contains("BIGINT"), "subsec is not cast to bigint: {pg}");
}

// === json_type(x) one-argument form: vocabulary refusal =====================

/// PostgreSQL's `jsonb_typeof` collapses SQLite's `'integer'` and `'real'`
/// into `'number'`, and `'true'` and `'false'` into `'boolean'`. No readable
/// static rewrite maps all six SQLite names faithfully, so the one-argument
/// form is refused with an explanation.
#[test]
fn json_type_one_arg_is_refused() {
    let err =
        rev("SELECT json_type(payload) FROM t").expect_err("json_type one-arg must be refused");
    let msg = err.to_string();
    // The message must name the vocabulary problem so the caller knows why.
    assert!(
        msg.contains("number") || msg.contains("boolean") || msg.contains("vocabulary"),
        "error must explain the vocabulary mismatch: {msg}"
    );
}

/// The two-argument form does path extraction and is not affected by the
/// one-argument refusal.
#[test]
fn json_type_two_arg_path_form_still_reverses() {
    assert_emits("SELECT json_type(payload, '$.a') FROM t", "jsonb_typeof");
}
