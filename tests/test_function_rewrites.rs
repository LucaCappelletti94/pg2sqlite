//! Tests for function translation rewrites: random(), NOW() window
//! preservation, to_timestamp(), and timestamp variant functions.

mod helpers;

use diesel::{RunQueryDsl, SqliteConnection, prelude::*};
use helpers::translate_sql;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// ============================================================================
// H1: random() semantic mismatch and ABS(min-int) overflow edge in SQLite.
// ============================================================================

#[test]
fn random_translates_to_float_range() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql("SELECT random()", &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("cast"), "expected CAST(random() AS REAL): {sql}");
    assert!(lower.contains("9223372036854775808"), "expected offset constant: {sql}");
    assert!(lower.contains("18446744073709551616"), "expected divisor constant: {sql}");
    assert!(!lower.contains("abs("), "random rewrite should avoid ABS to prevent overflow: {sql}");
    assert!(lower.contains('/'), "expected division operator: {sql}");
}

#[test]
fn random_rewrite_handles_sqlite_min_i64_without_overflow() -> Result<(), Box<dyn std::error::Error>>
{
    let sql = "SELECT random() AS val";
    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;
    let select_sql = translated[0].to_string();

    let forced_min_sql = if select_sql.contains("random()") {
        select_sql.replacen("random()", "-9223372036854775808", 1)
    } else if select_sql.contains("RANDOM()") {
        select_sql.replacen("RANDOM()", "-9223372036854775808", 1)
    } else {
        panic!("translated random SQL did not contain random() call: {select_sql}");
    };

    #[derive(QueryableByName, Debug)]
    struct FloatResult {
        #[diesel(sql_type = diesel::sql_types::Double)]
        val: f64,
    }

    let mut conn = SqliteConnection::establish(":memory:")?;
    let results = diesel::sql_query(&forced_min_sql).load::<FloatResult>(&mut conn)?;
    assert_eq!(results.len(), 1);
    assert!(
        results[0].val.is_finite(),
        "forced min-int rewrite result should be finite, got: {} (sql: {forced_min_sql})",
        results[0].val
    );

    Ok(())
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
