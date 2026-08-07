//! Tests for ordered-set and statistical aggregate functions.
//!
//! Covers:
//! - WITHIN GROUP ordered-set aggregates (percentile_cont, percentile_disc,
//!   mode)
//! - regr_* regression aggregate functions
//! - xmlagg, range_agg, multirange_agg
//! - string_agg with nested expressions and ORDER BY clause
//! - COUNT/SUM DISTINCT passthrough

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;
use run_translated_helper::run_translated_with;

#[path = "helpers/translate.rs"]
mod translate_helpers;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::ast::Statement;
use translate_helpers::translate_default_err as translate_err;

const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, val FLOAT, label TEXT, flag BOOLEAN);";

fn translate_ok(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .into_iter()
        .find(|s| matches!(s, Statement::Query(_)))
        .unwrap()
        .to_string()
}

/// percentile_cont uses WITHIN GROUP syntax unsupported by SQLite.
#[test]
fn percentile_cont_within_group_causes_error() {
    let err = translate_err(&format!(
        "{SCHEMA} SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY val) FROM t;"
    ));
    assert!(
        err.to_lowercase().contains("percentile"),
        "Error should mention percentile_cont: {err}"
    );
}

/// percentile_disc uses WITHIN GROUP syntax unsupported by SQLite.
#[test]
fn percentile_disc_within_group_causes_error() {
    let err = translate_err(&format!(
        "{SCHEMA} SELECT percentile_disc(0.5) WITHIN GROUP (ORDER BY val) FROM t;"
    ));
    assert!(
        err.to_lowercase().contains("percentile"),
        "Error should mention percentile_disc: {err}"
    );
}

/// mode() uses WITHIN GROUP syntax unsupported by SQLite.
#[test]
fn mode_within_group_causes_error() {
    let err = translate_err(&format!("{SCHEMA} SELECT mode() WITHIN GROUP (ORDER BY val) FROM t;"));
    assert!(err.to_lowercase().contains("mode"), "Error should mention mode: {err}");
}

#[test]
fn regr_slope_causes_error() {
    let err = translate_err(&format!("{SCHEMA} SELECT regr_slope(val, id::float) FROM t;"));
    assert!(err.to_lowercase().contains("regr"), "Error should mention regr: {err}");
}

#[test]
fn regr_intercept_causes_error() {
    let err = translate_err(&format!("{SCHEMA} SELECT regr_intercept(val, id::float) FROM t;"));
    assert!(err.to_lowercase().contains("regr"), "Error should mention regr: {err}");
}

#[test]
fn regr_r2_causes_error() {
    let err = translate_err(&format!("{SCHEMA} SELECT regr_r2(val, id::float) FROM t;"));
    assert!(err.to_lowercase().contains("regr"), "Error should mention regr: {err}");
}

#[test]
fn regr_count_causes_error() {
    let err = translate_err(&format!("{SCHEMA} SELECT regr_count(val, id::float) FROM t;"));
    assert!(err.to_lowercase().contains("regr"), "Error should mention regr: {err}");
}

#[test]
fn regr_avgx_causes_error() {
    let err = translate_err(&format!("{SCHEMA} SELECT regr_avgx(val, id::float) FROM t;"));
    assert!(err.to_lowercase().contains("regr"), "Error should mention regr: {err}");
}

#[test]
fn regr_avgy_causes_error() {
    let err = translate_err(&format!("{SCHEMA} SELECT regr_avgy(val, id::float) FROM t;"));
    assert!(err.to_lowercase().contains("regr"), "Error should mention regr: {err}");
}

#[test]
fn xmlagg_causes_error() {
    let err = translate_err(&format!("{SCHEMA} SELECT xmlagg(label) FROM t;"));
    assert!(err.to_lowercase().contains("xmlagg"), "Error should mention xmlagg: {err}");
}

#[test]
fn range_agg_causes_error() {
    let err = translate_err(&format!("{SCHEMA} SELECT range_agg(val::int4range) FROM t;"));
    assert!(err.to_lowercase().contains("range_agg"), "Error should mention range_agg: {err}");
}

#[test]
fn multirange_agg_causes_error() {
    let err = translate_err(&format!("{SCHEMA} SELECT multirange_agg(val::int4range) FROM t;"));
    assert!(
        err.to_lowercase().contains("multirange_agg"),
        "Error should mention multirange_agg: {err}"
    );
}

/// string_agg with ORDER BY clause inside the aggregate — the ORDER BY must
/// survive the rename to group_concat so SQLite can use it (requires SQLite
/// 3.44+).
#[test]
fn string_agg_with_order_by_preserves_order_clause() {
    let sql = format!("{SCHEMA} SELECT string_agg(label, ',' ORDER BY label) FROM t;");
    let out = translate_ok(&sql);
    assert!(out.contains("group_concat"), "Should rename to group_concat: {out}");
    assert!(out.to_uppercase().contains("ORDER BY"), "ORDER BY must be preserved: {out}");
    run_translated_with(&sql, &Pg2SqliteOptions::default());
}

/// string_agg(NOW()::text, ',') — the NOW() inside the aggregate arg must be
/// translated to datetime('now') in the output, not left as NOW().
#[test]
fn string_agg_translates_nested_now_in_args() {
    let sql = format!("{SCHEMA} SELECT string_agg(NOW()::text, ',') FROM t;");
    let out = translate_ok(&sql);
    assert!(out.contains("group_concat"), "Should rename to group_concat: {out}");
    assert!(
        out.to_lowercase().contains("datetime"),
        "NOW() inside arg must translate to datetime('now'): {out}"
    );
    assert!(
        !out.to_uppercase().contains("NOW()"),
        "NOW() must not appear verbatim in output: {out}"
    );
    run_translated_with(&sql, &Pg2SqliteOptions::default());
}

/// COUNT(DISTINCT col) passes through unchanged — SQLite supports this.
#[test]
fn count_distinct_passes_through() {
    let out = translate_ok(&format!("{SCHEMA} SELECT COUNT(DISTINCT val) FROM t;"));
    assert!(out.to_uppercase().contains("COUNT"), "COUNT should be present: {out}");
    assert!(out.to_uppercase().contains("DISTINCT"), "DISTINCT must be preserved: {out}");
    run_translated_with(
        &format!("{SCHEMA} SELECT COUNT(DISTINCT val) FROM t;"),
        &Pg2SqliteOptions::default(),
    );
}

/// SUM(DISTINCT col) passes through — SQLite supports aggregate DISTINCT.
#[test]
fn sum_distinct_passes_through() {
    let out = translate_ok(&format!("{SCHEMA} SELECT SUM(DISTINCT val) FROM t;"));
    assert!(out.to_uppercase().contains("SUM"), "SUM should be present: {out}");
    assert!(out.to_uppercase().contains("DISTINCT"), "DISTINCT must be preserved: {out}");
    run_translated_with(
        &format!("{SCHEMA} SELECT SUM(DISTINCT val) FROM t;"),
        &Pg2SqliteOptions::default(),
    );
}
