//! Tests for function translation rewrites: random(), NOW() window
//! preservation, to_timestamp(), and timestamp variant functions.

mod helpers;

use diesel::{RunQueryDsl, SqliteConnection, prelude::*};
use helpers::translate_sql;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

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
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    diesel::sql_query(&sql).execute(&mut conn).unwrap();
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

/// `random()` is rewritten onto SQLite's integer `random()` normalised into
/// PostgreSQL's `[0, 1)` domain. The properties asserted are the determinate
/// ones: every draw lands in the unit interval and 1000 draws cover it. The
/// chi-squared uniformity check is gone (R94): at p = 0.001 it failed a
/// correct implementation one run in a thousand by construction, and it was
/// testing SQLite's PRNG rather than this crate's translation.
#[test]
fn random_covers_the_unit_interval() -> Result<(), Box<dyn std::error::Error>> {
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
    }

    // Range coverage: with 1000 samples from U[0,1], the chance the minimum
    // stays above 0.05 or the maximum below 0.95 is under 1e-22 each, which
    // is beyond hardware failure rates rather than a once-in-a-thousand
    // false positive.
    assert!(
        min_val < 0.05,
        "minimum value {min_val} suspiciously high, the distribution may not cover the range"
    );
    assert!(
        max_val > 0.95,
        "maximum value {max_val} suspiciously low, the distribution may not cover the range"
    );

    Ok(())
}

/// Flipped R120 pin. `now() OVER (...)` is not PostgreSQL, which accepts OVER
/// only on a window or aggregate function, and the old passthrough emitted
/// `datetime('now') OVER (...)`, which SQLite refuses with `may not be used
/// as a window function`. The translator now refuses the input.
#[test]
fn now_with_an_over_clause_is_refused() {
    let options = Pg2SqliteOptions::default();
    let err =
        translate_sql("SELECT now() OVER (PARTITION BY department_id) FROM employees", &options)
            .expect_err("OVER on now() is not PostgreSQL");
    assert!(
        err.contains("now") && err.contains("OVER"),
        "the refusal should name the function and OVER: {err}"
    );
}

/// The guard the flipped pin above leaves behind: a function that IS a window
/// aggregate keeps its OVER clause through translation and executes.
#[test]
fn a_window_aggregate_keeps_its_over_clause() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql(
        "SELECT SUM(department_id) OVER (PARTITION BY department_id) FROM employees",
        &options,
    )
    .unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("over (partition by"), "OVER PARTITION BY should survive: {sql}");
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    // DDL: no typed DSL exists for CREATE TABLE in diesel.
    diesel::sql_query("CREATE TABLE employees (department_id INT)").execute(&mut conn).unwrap();
    // Dynamically generated translated SQL cannot be expressed via the typed DSL.
    diesel::sql_query(&sql).execute(&mut conn).expect("SUM OVER must run in SQLite");
}

#[test]
fn to_timestamp_epoch() {
    let options = Pg2SqliteOptions::default();
    let sql = translate_sql("SELECT to_timestamp(0)", &options).unwrap();
    let lower = sql.to_lowercase();
    assert!(lower.contains("datetime("), "expected datetime: {sql}");
    assert!(lower.contains("unixepoch"), "expected 'unixepoch' modifier: {sql}");
    let mut conn = SqliteConnection::establish(":memory:").unwrap();
    diesel::sql_query(&sql).execute(&mut conn).unwrap();
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

#[test]
fn to_timestamp_with_format_unsupported() {
    let options = Pg2SqliteOptions::default();
    let result = translate_sql("SELECT to_timestamp('2023-01-01', 'YYYY-MM-DD')", &options);
    assert!(result.is_err(), "to_timestamp with format should be unsupported");
}

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
        let mut conn = SqliteConnection::establish(":memory:").unwrap();
        diesel::sql_query(&sql).execute(&mut conn).unwrap();
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
