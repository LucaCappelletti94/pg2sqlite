//! R2-18: Interval notation refusals and unimplemented shapes.
//!
//! Verified on postgres:18-alpine:
//!   TIMESTAMP '2024-01-01 00:00:00' + 3 * INTERVAL '1 day'   -> 2024-01-04
//!   TIMESTAMP '2024-01-01 00:00:00' + INTERVAL '1 day' * 3   -> 2024-01-04
//!   INTERVAL '1 day' + TIMESTAMP '2024-01-01 00:00:00'        -> 2024-01-04
//!
//! Refusal contracts:
//!   ts + INTERVAL '01:30:00'   must name the verbose form (INTERVAL '1 hour 30
//! minutes')   ts + $1 * INTERVAL '1 day' must say the scalar cannot be folded
//! at translation time

mod helpers;

use diesel::{QueryableByName, RunQueryDsl, sql_query, sql_types};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn tr(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_iter()
        .next()
        .expect("at least one statement")
}

fn tr_err(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("must fail")
        .to_string()
}

#[derive(QueryableByName)]
struct ScalarText {
    #[diesel(sql_type = sql_types::Text)]
    r: String,
}

// --- R2-18a: INTERVAL on the left commutes -----------------------------------

/// PostgreSQL accepts `INTERVAL + TIMESTAMP` and commutes it to `TIMESTAMP +
/// INTERVAL`. The translator must do the same rather than erroring with
/// "INTERVAL not supported".
#[test]
fn interval_left_of_timestamp_commutes_and_executes() {
    // Measured on postgres:18-alpine: 2024-01-02
    let sql = tr("SELECT INTERVAL '1 day' + TIMESTAMP '2024-01-01 00:00:00' AS r");
    let mut conn = establish_connection();
    // sql_query: running dynamically generated translated SQL.
    let rows = sql_query(&sql).load::<ScalarText>(&mut conn).expect("commuted interval must run");
    assert!(
        rows[0].r.starts_with("2024-01-02"),
        "INTERVAL '1 day' + TIMESTAMP '2024-01-01' must yield 2024-01-02, got {}: {sql}",
        rows[0].r
    );
}

// --- R2-18b: scalar * INTERVAL constant-fold
// ----------------------------------

/// `TIMESTAMP + n * INTERVAL` folds the integer scalar into the modifier count.
/// Measured on postgres:18-alpine: 2024-01-04.
#[test]
fn scalar_times_interval_on_right_folds_to_modifier() {
    let sql = tr("SELECT TIMESTAMP '2024-01-01 00:00:00' + 3 * INTERVAL '1 day' AS r");
    let mut conn = establish_connection();
    // sql_query: running dynamically generated translated SQL.
    let rows =
        sql_query(&sql).load::<ScalarText>(&mut conn).expect("scalar * INTERVAL fold must run");
    assert!(
        rows[0].r.starts_with("2024-01-04"),
        "TIMESTAMP + 3 * INTERVAL '1 day' must yield 2024-01-04, got {}: {sql}",
        rows[0].r
    );
}

/// `TIMESTAMP + INTERVAL * n` (operands flipped inside the multiplication) also
/// folds. Measured on postgres:18-alpine: 2024-01-04.
#[test]
fn interval_times_scalar_on_right_folds_to_modifier() {
    let sql = tr("SELECT TIMESTAMP '2024-01-01 00:00:00' + INTERVAL '1 day' * 3 AS r");
    let mut conn = establish_connection();
    // sql_query: running dynamically generated translated SQL.
    let rows =
        sql_query(&sql).load::<ScalarText>(&mut conn).expect("INTERVAL * scalar fold must run");
    assert!(
        rows[0].r.starts_with("2024-01-04"),
        "TIMESTAMP + INTERVAL '1 day' * 3 must yield 2024-01-04, got {}: {sql}",
        rows[0].r
    );
}

// --- R2-18c: message contracts -----------------------------------------------

/// `INTERVAL '01:30:00'` (HH:MM:SS notation) is not decodable. The refusal must
/// name the supported verbose form so the caller can fix the SQL rather than
/// guessing. The old message falsely claimed INTERVAL is entirely unsupported.
#[test]
fn hh_mm_ss_interval_refusal_names_verbose_spelling() {
    let err = tr_err("SELECT TIMESTAMP '2024-01-01 00:00:00' + INTERVAL '01:30:00' AS r");
    assert!(
        err.to_lowercase().contains("hour") || err.contains("verbose") || err.contains("1 hour"),
        "refusal must name the verbose spelling (e.g. '1 hour 30 minutes'), got: {err}"
    );
    // Must not claim INTERVAL is entirely unsupported in SQLite.
    assert!(
        !err.contains("not supported in SQLite"),
        "must not claim INTERVAL is wholly unsupported; got: {err}"
    );
}

/// `ts + $1 * INTERVAL '1 day'` cannot be folded at translation time because
/// the scalar is a runtime parameter. The refusal must say so, not blame
/// INTERVAL.
#[test]
fn runtime_scalar_interval_refusal_names_folding_limit() {
    let err = tr_err("SELECT TIMESTAMP '2024-01-01 00:00:00' + $1 * INTERVAL '1 day' AS r");
    assert!(
        err.to_lowercase().contains("fold")
            || err.to_lowercase().contains("translation time")
            || err.to_lowercase().contains("runtime"),
        "refusal must say the scalar cannot be folded at translation time, got: {err}"
    );
}

// --- regression: existing verbose-form intervals still work ------------------

/// Plain verbose `INTERVAL '1 day'` arithmetic already works; this guards
/// against regressions in the new code paths.
#[test]
fn verbose_interval_arithmetic_still_translates() {
    let sql = tr("SELECT TIMESTAMP '2024-01-01 00:00:00' + INTERVAL '1 day' AS r");
    let mut conn = establish_connection();
    // sql_query: running dynamically generated translated SQL.
    let rows = sql_query(&sql).load::<ScalarText>(&mut conn).expect("verbose interval must run");
    assert!(
        rows[0].r.starts_with("2024-01-02"),
        "TIMESTAMP + INTERVAL '1 day' must yield 2024-01-02, got {}: {sql}",
        rows[0].r
    );
}
