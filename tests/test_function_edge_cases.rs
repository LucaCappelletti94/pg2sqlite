//! Tests for function translation edge cases in
//! `src/impls/translator_impls/function.rs`.
//!
//! Covers:
//! - CONCAT with zero args -> error
//! - CONCAT_WS with <2 args -> error
//! - CONCAT with single arg -> passthrough
//! - CONCAT_WS with separator + single value -> just the value
//! - FILTER clause on aggregate -> CASE WHEN transformation
//! - string_agg -> group_concat
//! - strpos -> INSTR
//! - chr -> char

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection as SqliteConn;

/// Helper: translate SQL and return the output or error string.
fn translate(sql: &str) -> Result<String, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
        .map_err(|e| e.to_string())
}

fn translate_ok(sql: &str) -> String {
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    stmts.iter().find(|s| matches!(s, sqlparser::ast::Statement::Query(_))).unwrap().to_string()
}

/// Executes all statements emitted by translating `sql` against a real
/// in-memory SQLite. Use after a substring assert to prove the output is
/// accepted by SQLite, not just parseable.
fn execute_all(sql: &str) {
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    let conn = SqliteConn::open_in_memory().unwrap();
    let script = stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n");
    conn.execute_batch(&script)
        .unwrap_or_else(|e| panic!("translated SQL must execute in SQLite: {e}\n{script}"));
}

#[test]
fn concat_zero_args_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               SELECT concat() FROM t;";
    let result = translate(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("CONCAT requires at least one argument"),
        "Expected CONCAT args error, got: {err}"
    );
}

#[test]
fn concat_single_arg_passthrough() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT concat(name) FROM t;";
    let output = translate(sql).unwrap();
    // With a single arg, CONCAT(name) should become just `name` (no || operator)
    assert!(!output.contains("||"), "Single arg CONCAT should not use ||, got: {output}");
    assert!(
        output.contains("name"),
        "Single arg CONCAT should pass through the expression, got: {output}"
    );
    execute_all(sql);
}

#[test]
fn concat_multiple_args_uses_string_concat() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, first_name TEXT, last_name TEXT);
               SELECT concat(first_name, ' ', last_name) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("||"), "Multi-arg CONCAT should use ||, got: {output}");
    execute_all(sql);
}

#[test]
fn concat_ws_less_than_two_args_produces_error() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               SELECT concat_ws(',') FROM t;";
    let result = translate(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("CONCAT_WS requires at least two arguments"),
        "Expected CONCAT_WS args error, got: {err}"
    );
}

#[test]
fn concat_ws_separator_plus_single_value() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT concat_ws(',', name) FROM t;";
    let output = translate(sql).unwrap();
    // With separator + single value, result is just the value (no || operator)
    assert!(!output.contains("||"), "CONCAT_WS with single value should not use ||, got: {output}");
    execute_all(sql);
}

#[test]
fn concat_ws_separator_plus_multiple_values() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, first_name TEXT, last_name TEXT);
               SELECT concat_ws(', ', first_name, last_name) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("||"), "CONCAT_WS with multiple values should use ||, got: {output}");
    execute_all(sql);
}

#[test]
fn filter_clause_on_count_star() {
    // SQLite 3.25 added native FILTER support; 3.46 is our floor, so the
    // translator keeps the FILTER clause rather than lowering to CASE WHEN.
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, x INT);
               SELECT COUNT(*) FILTER (WHERE x > 5) FROM t;";
    let output = translate(sql).unwrap();
    let lower = output.to_lowercase();
    assert!(lower.contains("filter"), "FILTER clause must be kept natively, got: {output}");
    assert!(!lower.contains("case when"), "CASE WHEN lowering must not happen, got: {output}");
    execute_all(sql);
}

#[test]
fn filter_clause_on_named_aggregate() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, x INT);
               SELECT SUM(x) FILTER (WHERE x > 0) FROM t;";
    let output = translate(sql).unwrap();
    let lower = output.to_lowercase();
    assert!(lower.contains("filter"), "FILTER clause must be kept natively, got: {output}");
    assert!(!lower.contains("case when"), "CASE WHEN lowering must not happen, got: {output}");
    execute_all(sql);
}

#[test]
fn string_agg_becomes_group_concat() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT string_agg(name, ', ') FROM t;";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("group_concat"),
        "string_agg should become group_concat, got: {output}"
    );
    assert!(
        !output.to_lowercase().contains("string_agg"),
        "string_agg should be replaced, got: {output}"
    );
    execute_all(sql);
}

#[test]
fn strpos_becomes_instr() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT strpos(name, 'test') FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("INSTR"), "strpos should become INSTR, got: {output}");
    execute_all(sql);
}

#[test]
fn chr_becomes_char() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               SELECT chr(65) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("char"), "chr should become char, got: {output}");
    // Make sure "chr" is not present
    assert!(!output.contains("chr("), "chr should be renamed to char, got: {output}");
    execute_all(sql);
}

#[test]
fn least_becomes_min() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);
               SELECT least(a, b) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("MIN"), "LEAST should become MIN, got: {output}");
    execute_all(sql);
}

#[test]
fn greatest_becomes_max() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);
               SELECT greatest(a, b) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("MAX"), "GREATEST should become MAX, got: {output}");
    execute_all(sql);
}

#[test]
fn least_still_translates_correctly() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE le_test (id INT PRIMARY KEY, a INTEGER NOT NULL, b INTEGER NOT NULL);
               SELECT LEAST(a, b) AS result FROM le_test;";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();
    assert!(
        select_sql.to_uppercase().contains("MIN("),
        "LEAST should still become MIN, got: {select_sql}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    diesel::sql_query("INSERT INTO le_test VALUES (1, 3, 7)").execute(&mut conn)?;

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        result: i32,
    }
    let rows = diesel::sql_query(select_sql).load::<Row>(&mut conn)?;
    assert_eq!(rows[0].result, 3, "LEAST(3, 7) should return 3");
    Ok(())
}

#[test]
fn filter_rewrite_count_with_filter() {
    let sql = "CREATE TABLE t2 (id INT PRIMARY KEY, x INT, y INT);
               SELECT COUNT(x) FILTER (WHERE y > 0) FROM t2;";
    let output = translate(sql).unwrap();
    let lower = output.to_lowercase();
    assert!(lower.contains("filter"), "FILTER clause must be kept natively: {output}");
    assert!(!lower.contains("case when"), "CASE WHEN lowering must not happen: {output}");
    execute_all(sql);
}

#[test]
fn filter_rewrite_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let pg_sql = "
        CREATE TABLE scores (id SERIAL PRIMARY KEY, value INTEGER NOT NULL);
        SELECT COUNT(value) FILTER (WHERE value > 5) AS high_count FROM scores;
    ";
    let translated = Pg2Sqlite::default().sql(pg_sql)?.translate(&Pg2SqliteOptions::default())?;

    let mut conn = SqliteConnection::establish(":memory:")?;

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    diesel::sql_query(
        "INSERT INTO scores (id, value) VALUES (1, 3), (2, 7), (3, 10), (4, 2), (5, 8)",
    )
    .execute(&mut conn)?;

    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();

    #[derive(diesel::QueryableByName, Debug)]
    struct CountResult {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        high_count: i64,
    }

    let results = diesel::sql_query(&select_sql).load::<CountResult>(&mut conn)?;
    assert_eq!(results.len(), 1);
    // values > 5: 7, 10, 8 = 3
    assert_eq!(results[0].high_count, 3, "expected 3 values > 5");

    Ok(())
}

const SIMPLE_TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, val INT, flag BOOLEAN);";

#[test]
fn json_agg_translates_to_json_group_array() {
    let sql = format!("{SIMPLE_TABLE} SELECT json_agg(val) FROM t;");
    let out = translate_ok(&sql);
    assert!(out.contains("json_group_array"), "json_agg should become json_group_array: {out}");
    assert!(!out.contains("json_agg"), "Should not contain json_agg: {out}");
    execute_all(&sql);
}

#[test]
fn jsonb_agg_translates_to_json_group_array() {
    let sql = format!("{SIMPLE_TABLE} SELECT jsonb_agg(val) FROM t;");
    let out = translate_ok(&sql);
    assert!(out.contains("json_group_array"), "jsonb_agg should become json_group_array: {out}");
    assert!(!out.contains("jsonb_agg"), "Should not contain jsonb_agg: {out}");
    execute_all(&sql);
}

#[test]
fn json_object_agg_translates_to_json_group_object() {
    let sql = format!("{SIMPLE_TABLE} SELECT json_object_agg(id, val) FROM t;");
    let out = translate_ok(&sql);
    assert!(
        out.contains("json_group_object"),
        "json_object_agg should become json_group_object: {out}"
    );
    assert!(!out.contains("json_object_agg"), "Should not contain json_object_agg: {out}");
    execute_all(&sql);
}

#[test]
fn jsonb_object_agg_translates_to_json_group_object() {
    let sql = format!("{SIMPLE_TABLE} SELECT jsonb_object_agg(id, val) FROM t;");
    let out = translate_ok(&sql);
    assert!(
        out.contains("json_group_object"),
        "jsonb_object_agg should become json_group_object: {out}"
    );
    assert!(!out.contains("jsonb_object_agg"), "Should not contain jsonb_object_agg: {out}");
    execute_all(&sql);
}

#[test]
fn now_becomes_datetime_now() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               SELECT now();";
    let output = translate(sql).unwrap();
    assert!(output.contains("datetime"), "NOW() should become datetime('now'), got: {output}");
    execute_all(sql);
}

#[test]
fn schema_qualified_now_becomes_datetime_now() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               SELECT pg_catalog.now();";
    let output = translate(sql).unwrap();
    assert!(
        output.contains("datetime"),
        "pg_catalog.now() should become datetime('now'), got: {output}"
    );
    assert!(
        !output.to_lowercase().contains("pg_catalog.now"),
        "schema-qualified NOW should be rewritten, got: {output}"
    );
    execute_all(sql);
}

// ── H2: json_object_agg / jsonb_object_agg over an empty set ────────────────

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

/// H2: PostgreSQL returns NULL when json_object_agg finds no rows. The current
/// emission of bare json_group_object(k, v) returns '{}' instead.
#[test]
fn json_object_agg_over_empty_set_returns_null() {
    let rows = run_translated_helper::run_translated_with(
        "CREATE TABLE kv (k TEXT NOT NULL, v INT NOT NULL);
         SELECT json_object_agg(k, v) FROM kv;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![None], "json_object_agg over empty set should be NULL, got: {rows:?}");
}

/// Same defect for the jsonb_ spelling.
#[test]
fn jsonb_object_agg_over_empty_set_returns_null() {
    let rows = run_translated_helper::run_translated_with(
        "CREATE TABLE kv (k TEXT NOT NULL, v INT NOT NULL);
         SELECT jsonb_object_agg(k, v) FROM kv;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![None], "jsonb_object_agg over empty set should be NULL, got: {rows:?}");
}

/// Green companion: a non-empty set returns a JSON object containing the key.
#[test]
fn json_object_agg_over_nonempty_set_returns_json_object() {
    let rows = run_translated_helper::run_translated_with(
        "CREATE TABLE kv (k TEXT NOT NULL, v INT NOT NULL);
         INSERT INTO kv VALUES ('answer', 42);
         SELECT json_object_agg(k, v) FROM kv;",
        &Pg2SqliteOptions::default(),
    );
    let text = rows.into_iter().next().flatten().expect("non-empty result must not be NULL");
    assert!(text.contains("answer"), "result must contain the key: {text}");
}

/// Green companion for the jsonb_ spelling.
#[test]
fn jsonb_object_agg_over_nonempty_set_returns_json_object() {
    let rows = run_translated_helper::run_translated_with(
        "CREATE TABLE kv (k TEXT NOT NULL, v INT NOT NULL);
         INSERT INTO kv VALUES ('answer', 42);
         SELECT jsonb_object_agg(k, v) FROM kv;",
        &Pg2SqliteOptions::default(),
    );
    let text = rows.into_iter().next().flatten().expect("non-empty result must not be NULL");
    assert!(text.contains("answer"), "result must contain the key: {text}");
}

// ── R2-7: FILTER on json_agg/json_object_agg must not collect NULLs ─────────

/// json_agg(x) FILTER (WHERE x > 0) over (1, -1) must give [1], not [1,null].
/// CASE WHEN lowering stuffs a NULL for the excluded row into json_group_array.
#[test]
fn json_agg_filter_does_not_collect_nulls() {
    let rows = run_translated_helper::run_translated_with(
        "CREATE TABLE t (x INT);
         INSERT INTO t VALUES (1), (-1);
         SELECT json_agg(x) FILTER (WHERE x > 0) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(
        rows,
        vec![Some("[1]".to_string())],
        "json_agg FILTER must exclude -1, got: {rows:?}",
    );
}
