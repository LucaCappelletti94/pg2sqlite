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
}

#[test]
fn concat_multiple_args_uses_string_concat() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, first_name TEXT, last_name TEXT);
               SELECT concat(first_name, ' ', last_name) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("||"), "Multi-arg CONCAT should use ||, got: {output}");
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
}

#[test]
fn concat_ws_separator_plus_multiple_values() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, first_name TEXT, last_name TEXT);
               SELECT concat_ws(', ', first_name, last_name) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("||"), "CONCAT_WS with multiple values should use ||, got: {output}");
}

#[test]
fn filter_clause_on_count_star() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, x INT);
               SELECT COUNT(*) FILTER (WHERE x > 5) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("CASE WHEN"), "FILTER should become CASE WHEN, got: {output}");
    assert!(!output.contains("FILTER"), "FILTER clause should be removed, got: {output}");
}

#[test]
fn filter_clause_on_named_aggregate() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, x INT);
               SELECT SUM(x) FILTER (WHERE x > 0) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("CASE WHEN"), "FILTER should become CASE WHEN, got: {output}");
    assert!(!output.contains("FILTER"), "FILTER clause should be removed, got: {output}");
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
}

#[test]
fn strpos_becomes_instr() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
               SELECT strpos(name, 'test') FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("INSTR"), "strpos should become INSTR, got: {output}");
}

#[test]
fn chr_becomes_char() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               SELECT chr(65) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("char"), "chr should become char, got: {output}");
    // Make sure "chr" is not present
    assert!(!output.contains("chr("), "chr should be renamed to char, got: {output}");
}

#[test]
fn least_becomes_min() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);
               SELECT least(a, b) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("MIN"), "LEAST should become MIN, got: {output}");
}

#[test]
fn greatest_becomes_max() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);
               SELECT greatest(a, b) FROM t;";
    let output = translate(sql).unwrap();
    assert!(output.contains("MAX"), "GREATEST should become MAX, got: {output}");
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
    assert!(lower.contains("case when"), "expected CASE WHEN from FILTER rewrite: {output}");
    assert!(!lower.contains("filter"), "FILTER clause should be removed: {output}");
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
}

#[test]
fn jsonb_agg_translates_to_json_group_array() {
    let sql = format!("{SIMPLE_TABLE} SELECT jsonb_agg(val) FROM t;");
    let out = translate_ok(&sql);
    assert!(out.contains("json_group_array"), "jsonb_agg should become json_group_array: {out}");
    assert!(!out.contains("jsonb_agg"), "Should not contain jsonb_agg: {out}");
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
}

#[test]
fn now_becomes_datetime_now() {
    let sql = "CREATE TABLE t (id INT PRIMARY KEY);
               SELECT now();";
    let output = translate(sql).unwrap();
    assert!(output.contains("datetime"), "NOW() should become datetime('now'), got: {output}");
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
}
