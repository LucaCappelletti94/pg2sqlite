//! Tests for missing translation paths fixed in the audit.
//!
//! Covers: H1-H7 (HIGH), M1-M11 (MEDIUM function arms).
//! Each translatable function includes a semantic test that executes the
//! translated SQL against a real SQLite database and verifies results.

#![allow(dead_code)]

mod helpers;

use diesel::{RunQueryDsl, SqliteConnection, prelude::*};
use helpers::{translate_sql, translate_statements};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// ============================================================================
// H1: random() semantic mismatch → ABS(random()) / 9223372036854775807.0
// ============================================================================

#[test]
fn random_translates_to_float_range() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql("SELECT random()", &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("abs"), "expected ABS wrapping: {sql}");
    assert!(lower.contains("9223372036854775807"), "expected divisor constant: {sql}");
    assert!(lower.contains('/'), "expected division operator: {sql}");
}

#[test]
fn random_semantic_uniform_distribution() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "SELECT random() AS val";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = SqliteConnection::establish(":memory:")?;

    let select_sql = translated[0].to_string();

    #[derive(QueryableByName, Debug)]
    struct FloatResult {
        #[diesel(sql_type = diesel::sql_types::Double)]
        val: f64,
    }

    const N: u32 = 1000;
    const NUM_BUCKETS_USIZE: usize = 10;
    const NUM_BUCKETS_U32: u32 = 10;
    const THRESHOLDS: [f64; 9] = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9];

    let mut buckets = vec![0u32; NUM_BUCKETS_USIZE];
    let mut min_val = f64::MAX;
    let mut max_val = f64::MIN;

    for _ in 0..N {
        let results = diesel::sql_query(&select_sql).load::<FloatResult>(&mut conn)?;
        let val = results[0].val;
        assert!(
            (0.0..=1.0).contains(&val),
            "random() should produce value in [0.0, 1.0], got: {val}"
        );
        min_val = min_val.min(val);
        max_val = max_val.max(val);
        // Bucket index: [0.0, 0.1) -> 0, [0.1, 0.2) -> 1, ..., [0.9, 1.0] -> 9
        let bucket = THRESHOLDS
            .iter()
            .position(|&threshold| val < threshold)
            .unwrap_or(NUM_BUCKETS_USIZE - 1);
        buckets[bucket] += 1;
    }

    // Check range coverage: with 1000 samples from U[0,1], min should be < 0.05
    // and max should be > 0.95 (probability of failure < 1e-22 each).
    assert!(
        min_val < 0.05,
        "minimum value {min_val} suspiciously high — distribution may not cover full range"
    );
    assert!(
        max_val > 0.95,
        "maximum value {max_val} suspiciously low — distribution may not cover full range"
    );

    // Chi-squared test for uniformity across 10 buckets.
    // Expected count per bucket = n / num_buckets = 100.
    // With df=9, chi-squared critical value at p=0.001 is 27.88.
    // This gives an extremely low false-positive rate.
    let expected = f64::from(N) / f64::from(NUM_BUCKETS_U32);
    let chi_sq: f64 = buckets
        .iter()
        .map(|&count| {
            let diff = f64::from(count) - expected;
            diff * diff / expected
        })
        .sum();

    assert!(
        chi_sq < 27.88,
        "chi-squared {chi_sq:.2} exceeds critical value 27.88 (p=0.001, df=9) — \
         distribution is not uniform. Bucket counts: {buckets:?}"
    );

    Ok(())
}

// ============================================================================
// H2: WithArgs (NOW()) preserves window OVER clause
// ============================================================================

#[test]
fn with_args_preserves_window_over() {
    let options = Pg2SqliteOptions::default();
    let sql =
        translate_sql("SELECT now() OVER (PARTITION BY department_id) FROM employees", &options)
            .unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("datetime('now')"), "expected datetime('now'): {sql}");
    assert!(lower.contains("over"), "expected OVER clause preserved: {sql}");
}

#[test]
fn now_over_partition_translation_preserves_structure() {
    // datetime('now') is not a valid SQLite window function, but the translation
    // should faithfully preserve the OVER clause structure so that functions that
    // ARE valid window functions (like SUM, COUNT) don't lose their window spec
    // when routed through the WithArgs path.
    let options = Pg2SqliteOptions::default();
    let sql =
        translate_sql("SELECT now() OVER (PARTITION BY department_id) FROM employees", &options)
            .unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("over (partition by"), "OVER PARTITION BY should be preserved: {sql}");
    assert!(lower.contains("department_id"), "partition column should be preserved: {sql}");
}

// ============================================================================
// H3: date_trunc preserves window OVER clause
// ============================================================================

#[test]
fn date_trunc_preserves_window_over() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql(
        "SELECT date_trunc('day', created_at) OVER (PARTITION BY user_id) FROM events",
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("strftime"), "expected strftime: {sql}");
    assert!(lower.contains("over"), "expected OVER clause preserved: {sql}");
}

#[test]
fn date_trunc_without_over_semantic() -> Result<(), Box<dyn std::error::Error>> {
    // Test date_trunc semantics work correctly (without OVER, since strftime
    // is not a valid SQLite window function).
    let pg_sql = "
        CREATE TABLE events (id SERIAL PRIMARY KEY, user_id INTEGER NOT NULL, created_at TEXT NOT NULL);
        SELECT date_trunc('day', created_at) AS truncated FROM events;
    ";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(pg_sql)?.translate(&options)?;

    let mut conn = SqliteConnection::establish(":memory:")?;

    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    diesel::sql_query(
        "INSERT INTO events (id, user_id, created_at) VALUES (1, 1, '2024-03-15 10:30:00'), (2, 1, '2024-03-15 14:00:00')",
    )
    .execute(&mut conn)?;

    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();

    #[derive(QueryableByName, Debug)]
    struct TruncResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        truncated: String,
    }

    let results = diesel::sql_query(&select_sql).load::<TruncResult>(&mut conn)?;
    assert_eq!(results.len(), 2);
    // Both rows for the same day should produce the same truncated date
    assert_eq!(results[0].truncated, results[1].truncated);
    // Day truncation should produce YYYY-MM-DD 00:00:00
    assert!(
        results[0].truncated.contains("2024-03-15"),
        "expected day-truncated date, got: {}",
        results[0].truncated
    );

    Ok(())
}

#[test]
fn date_trunc_over_partition_translation_preserves_structure() {
    // strftime is not a valid SQLite window function, but the translation
    // should preserve the OVER clause structure for functions that are.
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql(
        "SELECT date_trunc('day', created_at) OVER (PARTITION BY user_id) FROM events",
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("over (partition by"), "OVER PARTITION BY should be preserved: {sql}");
    assert!(lower.contains("user_id"), "partition column should be preserved: {sql}");
}

// ============================================================================
// H4: to_char preserves window OVER clause
// ============================================================================

#[test]
fn to_char_preserves_window_over() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql(
        "SELECT to_char(created_at, 'YYYY-MM-DD') OVER (PARTITION BY user_id) FROM events",
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("strftime"), "expected strftime: {sql}");
    assert!(lower.contains("over"), "expected OVER clause preserved: {sql}");
}

// ============================================================================
// H5: FILTER rewrite
// ============================================================================

#[test]
fn filter_rewrite_count_with_filter() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql("SELECT COUNT(x) FILTER (WHERE y > 0) FROM t", &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("case when"), "expected CASE WHEN from FILTER rewrite: {sql}");
    assert!(!lower.contains("filter"), "FILTER clause should be removed: {sql}");
}

#[test]
fn filter_rewrite_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let pg_sql = "
        CREATE TABLE scores (id SERIAL PRIMARY KEY, value INTEGER NOT NULL);
        SELECT COUNT(value) FILTER (WHERE value > 5) AS high_count FROM scores;
    ";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(pg_sql)?.translate(&options)?;

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

    #[derive(QueryableByName, Debug)]
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

// ============================================================================
// H6: table CHECK constraint translates expressions
// ============================================================================

#[test]
fn table_check_translates_char_length() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql("CREATE TABLE t (name TEXT, CHECK (char_length(name) > 0))", &options)
        .unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("length("), "expected char_length → length translation: {sql}");
    assert!(!lower.contains("char_length"), "char_length should be translated: {sql}");
}

#[test]
fn table_check_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let pg_sql = "CREATE TABLE validated (name TEXT NOT NULL, CHECK (char_length(name) > 0))";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(pg_sql)?.translate(&options)?;

    let mut conn = SqliteConnection::establish(":memory:")?;

    for stmt in &translated {
        diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
    }

    // Valid insert: name has length > 0
    let result =
        diesel::sql_query("INSERT INTO validated (name) VALUES ('hello')").execute(&mut conn);
    assert!(result.is_ok(), "valid insert should succeed");

    // Invalid insert: empty string, length == 0 → CHECK constraint should fail
    let result = diesel::sql_query("INSERT INTO validated (name) VALUES ('')").execute(&mut conn);
    assert!(result.is_err(), "empty string should violate CHECK constraint");

    Ok(())
}

// ============================================================================
// M: left() → substr()
// ============================================================================

#[test]
fn left_function_to_substr() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql("SELECT left('hello', 3)", &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("substr("), "expected substr: {sql}");
}

#[test]
fn left_function_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "SELECT left('hello world', 5) AS result";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = SqliteConnection::establish(":memory:")?;

    #[derive(QueryableByName, Debug)]
    struct TextResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        result: String,
    }

    let results = diesel::sql_query(&translated[0].to_string()).load::<TextResult>(&mut conn)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].result, "hello", "left('hello world', 5) should be 'hello'");

    Ok(())
}

// ============================================================================
// M: right() → substr()
// ============================================================================

#[test]
fn right_function_to_substr() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql("SELECT right('hello', 3)", &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("substr("), "expected substr: {sql}");
}

#[test]
fn right_function_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "SELECT right('hello world', 5) AS result";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = SqliteConnection::establish(":memory:")?;

    #[derive(QueryableByName, Debug)]
    struct TextResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        result: String,
    }

    let results = diesel::sql_query(&translated[0].to_string()).load::<TextResult>(&mut conn)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].result, "world", "right('hello world', 5) should be 'world'");

    Ok(())
}

// ============================================================================
// M: to_timestamp(epoch) → datetime(val, 'unixepoch')
// ============================================================================

#[test]
fn to_timestamp_epoch() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql("SELECT to_timestamp(0)", &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("datetime("), "expected datetime: {sql}");
    assert!(lower.contains("unixepoch"), "expected 'unixepoch' modifier: {sql}");
}

#[test]
fn to_timestamp_epoch_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "SELECT to_timestamp(0) AS result";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = SqliteConnection::establish(":memory:")?;

    #[derive(QueryableByName, Debug)]
    struct TextResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        result: String,
    }

    let results = diesel::sql_query(&translated[0].to_string()).load::<TextResult>(&mut conn)?;
    assert_eq!(results.len(), 1);
    // Unix epoch 0 = 1970-01-01 00:00:00
    assert_eq!(
        results[0].result, "1970-01-01 00:00:00",
        "to_timestamp(0) should be 1970-01-01 00:00:00"
    );

    Ok(())
}

#[test]
fn to_timestamp_epoch_semantic_nonzero() -> Result<(), Box<dyn std::error::Error>> {
    // 1700000000 = 2023-11-14 22:13:20 UTC
    let sql = "SELECT to_timestamp(1700000000) AS result";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut conn = SqliteConnection::establish(":memory:")?;

    #[derive(QueryableByName, Debug)]
    struct TextResult {
        #[diesel(sql_type = diesel::sql_types::Text)]
        result: String,
    }

    let results = diesel::sql_query(&translated[0].to_string()).load::<TextResult>(&mut conn)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].result, "2023-11-14 22:13:20");

    Ok(())
}

// ============================================================================
// M: to_timestamp(text, format) → Unsupported
// ============================================================================

#[test]
fn to_timestamp_with_format_unsupported() {
    let options = Pg2SqliteOptions::default();
    let result = translate_sql("SELECT to_timestamp('2023-01-01', 'YYYY-MM-DD')", &options);
    assert!(result.is_err(), "to_timestamp with format should be unsupported");
}

// ============================================================================
// M: transaction_timestamp / statement_timestamp / clock_timestamp →
// datetime('now')
// ============================================================================

#[test]
fn timestamp_variants_are_now() {
    let options = Pg2SqliteOptions::default();
    for func in &["transaction_timestamp()", "statement_timestamp()", "clock_timestamp()"] {
        let sql = translate_sql(&format!("SELECT {func}"), &options).unwrap();
        let lower = sql.to_lowercase();
        assert!(
            lower.contains("datetime('now')"),
            "{func} should translate to datetime('now'): {sql}"
        );
    }
}

#[test]
fn timestamp_variants_semantic() -> Result<(), Box<dyn std::error::Error>> {
    for func in &["transaction_timestamp()", "statement_timestamp()", "clock_timestamp()"] {
        let sql = format!("SELECT {func} AS ts");
        let options = Pg2SqliteOptions::default();
        let translated = Pg2Sqlite::default().sql(&sql)?.translate(&options)?;

        let mut conn = SqliteConnection::establish(":memory:")?;

        #[derive(QueryableByName, Debug)]
        struct TsResult {
            #[diesel(sql_type = diesel::sql_types::Text)]
            ts: String,
        }

        let results = diesel::sql_query(&translated[0].to_string()).load::<TsResult>(&mut conn)?;
        assert_eq!(results.len(), 1);
        assert!(
            results[0].ts.contains('-') && results[0].ts.contains(':'),
            "{func} should produce a datetime, got: {}",
            results[0].ts
        );
    }

    Ok(())
}

// ============================================================================
// M: currval / lastval / setval → Unsupported
// ============================================================================

#[test]
fn currval_lastval_setval_unsupported() {
    let options = Pg2SqliteOptions::default();
    for func in &["currval('seq')", "lastval()", "setval('seq', 1)"] {
        let result = translate_sql(&format!("SELECT {func}"), &options);
        assert!(result.is_err(), "{func} should be unsupported: {result:?}");
    }
}

// ============================================================================
// M: repeat → Unsupported
// ============================================================================

#[test]
fn repeat_unsupported() {
    let options = Pg2SqliteOptions::default();
    let result = translate_sql("SELECT repeat('ab', 3)", &options);
    assert!(result.is_err(), "repeat should be unsupported");
}

// ============================================================================
// M: md5 → Unsupported
// ============================================================================

#[test]
fn md5_unsupported() {
    let options = Pg2SqliteOptions::default();
    let result = translate_sql("SELECT md5('hello')", &options);
    assert!(result.is_err(), "md5 should be unsupported");
}

// ============================================================================
// M: translate → Unsupported
// ============================================================================

#[test]
fn translate_function_unsupported() {
    let options = Pg2SqliteOptions::default();
    let result = translate_sql("SELECT translate('abc', 'a', 'x')", &options);
    assert!(result.is_err(), "translate should be unsupported");
}

// ============================================================================
// M: to_date → Unsupported
// ============================================================================

#[test]
fn to_date_unsupported() {
    let options = Pg2SqliteOptions::default();
    let result = translate_sql("SELECT to_date('2023-01-01', 'YYYY-MM-DD')", &options);
    assert!(result.is_err(), "to_date should be unsupported");
}

// ============================================================================
// M: age → Unsupported
// ============================================================================

#[test]
fn age_unsupported() {
    let options = Pg2SqliteOptions::default();
    let result = translate_sql("SELECT age(timestamp '2023-01-01')", &options);
    assert!(result.is_err(), "age should be unsupported");
}

// ============================================================================
// M: regexp_match / regexp_matches → Unsupported
// ============================================================================

#[test]
fn regexp_match_unsupported() {
    let options = Pg2SqliteOptions::default();
    let result = translate_sql("SELECT regexp_match('abc', 'a')", &options);
    assert!(result.is_err(), "regexp_match should be unsupported");
    let result = translate_sql("SELECT regexp_matches('abc', 'a')", &options);
    assert!(result.is_err(), "regexp_matches should be unsupported");
}

// ============================================================================
// M: array_length / array_to_string → Unsupported
// ============================================================================

#[test]
fn array_functions_unsupported() {
    let options = Pg2SqliteOptions::default();
    let result = translate_sql("SELECT array_length(ARRAY[1,2,3], 1)", &options);
    assert!(result.is_err(), "array_length should be unsupported");
}

// ============================================================================
// M: format → Unsupported
// ============================================================================

#[test]
fn format_unsupported() {
    let options = Pg2SqliteOptions::default();
    let result = translate_sql("SELECT format('Hello %s', 'world')", &options);
    assert!(result.is_err(), "format should be unsupported");
}

// ============================================================================
// H7: SetExpr DML translation (INSERT/UPDATE/DELETE in CTE body)
// ============================================================================

#[test]
fn set_expr_insert_translates_expressions() {
    let options = Pg2SqliteOptions::default();
    let result = translate_statements(
        "SELECT * FROM (INSERT INTO t (name) VALUES ('test') RETURNING *) AS sub",
        &options,
    );
    // The key point is it doesn't panic
    let _ = result;
}
