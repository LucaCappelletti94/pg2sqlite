//! The nine statistical aggregates pass through to the destination's own
//! implementation, and refuse when the caller has not declared one.
//!
//! SQLite has none of them. They used to be lowered onto closed forms over
//! `avg`, `sum` and `count`, which cancel catastrophically: measured against
//! PostgreSQL 17, ten money values one cent apart around 123456789.01 gave
//! 2.0 where PostgreSQL gives 0.000824999, and fifty values of `1e9 + random()`
//! drove the population variance to -128, so the standard deviation over them
//! came back NULL. The closed forms also took the argument out of the call and
//! rebuilt a bare aggregate, which discarded `OVER` and `DISTINCT`.
//!
//! Every expected number below was read off PostgreSQL 17 in Docker before the
//! change went in.

// The row count of a fixture crosses into `f64` so the expected variance can
// be computed alongside the one SQLite answers.
#![allow(clippy::cast_precision_loss)]

#[path = "helpers/statistical_aggregates.rs"]
mod statistical_aggregates;

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};
use rusqlite::Connection;
use statistical_aggregates::{STATISTICAL_AGGREGATES, register_statistical_aggregates};

/// Options declaring all nine, which is what a caller who registered them
/// writes.
fn declared() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_user_defined_functions(STATISTICAL_AGGREGATES.iter().copied())
}

fn translate(pg: &str, options: &Pg2SqliteOptions) -> Vec<String> {
    Pg2Sqlite::default().sql(pg).expect("parse").translate_to_sql(options).expect("translate")
}

/// Applies every emitted statement but the last against a connection carrying
/// the nine, then returns the last statement's first column for every row.
fn evaluate(pg: &str) -> Vec<Option<f64>> {
    let mut statements = translate(pg, &declared());
    let probe = statements.pop().expect("a probe statement");
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    register_statistical_aggregates(&connection).expect("register the aggregates");
    for statement in &statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }
    let mut prepared = connection
        .prepare(&probe)
        .unwrap_or_else(|error| panic!("emitted probe failed to prepare: {error}\n{probe}"));
    prepared
        .query_map([], |row| row.get::<_, Option<f64>>(0))
        .expect("query the probe")
        .collect::<Result<Vec<_>, _>>()
        .expect("read every row")
}

/// The single value of a one-row probe.
fn evaluate_one(pg: &str) -> Option<f64> {
    let rows = evaluate(pg);
    assert_eq!(rows.len(), 1, "expected exactly one row from:\n{pg}");
    rows[0]
}

fn close(left: f64, right: f64) -> bool {
    (left - right).abs() <= 1e-12 * right.abs().max(1.0)
}

// ---------- the refusal ----------

#[test]
fn an_undeclared_statistical_aggregate_is_refused() {
    for name in STATISTICAL_AGGREGATES {
        let arguments =
            if ["covar_pop", "covar_samp", "corr"].contains(name) { "x, y" } else { "x" };
        let error = Pg2Sqlite::default()
            .sql(&format!("CREATE TABLE t (x REAL, y REAL); SELECT {name}({arguments}) FROM t;"))
            .expect("parse")
            .translate_to_sql(&Pg2SqliteOptions::default())
            .expect_err("an undeclared statistical aggregate must be refused");
        let message = error.to_string();
        assert!(message.contains(*name), "{name}: message must name it: {message}");
        assert!(
            message.contains("with_user_defined_functions"),
            "{name}: message must name the remedy: {message}"
        );
    }
}

/// The closed forms are gone, so a bare `avg`/`sum` shape must never be
/// emitted again. This is what would silently return to the wrong numbers.
#[test]
fn no_statistical_aggregate_is_lowered_onto_avg_or_sum() {
    for name in STATISTICAL_AGGREGATES {
        let arguments =
            if ["covar_pop", "covar_samp", "corr"].contains(name) { "x, y" } else { "x" };
        let emitted = translate(
            &format!("CREATE TABLE t (x REAL, y REAL); SELECT {name}({arguments}) FROM t;"),
            &declared(),
        )
        .pop()
        .expect("a probe");
        assert_eq!(emitted, format!("SELECT {name}({arguments}) FROM t"), "{name}");
    }
}

/// The four square-root forms were gated on the math-functions option purely
/// because their closed form called `sqrt`. Nothing emits `sqrt` now.
#[test]
fn the_square_root_forms_no_longer_need_the_math_functions_option() {
    for name in ["stddev", "stddev_pop", "stddev_samp", "corr"] {
        let arguments = if name == "corr" { "x, y" } else { "x" };
        let options = Pg2SqliteOptions::default().with_user_defined_functions([name]);
        let emitted = translate(
            &format!("CREATE TABLE t (x REAL, y REAL); SELECT {name}({arguments}) FROM t;"),
            &options,
        )
        .pop()
        .expect("a probe");
        assert!(!emitted.contains("sqrt"), "{name} must not call sqrt: {emitted}");
    }
}

// ---------- the numbers the closed form got wrong ----------

const CLUSTERED: &str = "CREATE TABLE t (x DOUBLE PRECISION);
     INSERT INTO t VALUES (1000000000), (1000000001), (1000000002);";

/// The item's own trigger. PostgreSQL 17 answers 0.6666666666666666, the
/// closed form answered 0 exactly.
#[test]
fn population_variance_over_a_tight_cluster_matches_postgresql() {
    let answer = evaluate_one(&format!("{CLUSTERED} SELECT var_pop(x) FROM t;")).expect("a number");
    assert!(close(answer, 0.666_666_666_666_666_6), "got {answer}");
}

#[test]
fn sample_variance_over_a_tight_cluster_matches_postgresql() {
    let answer =
        evaluate_one(&format!("{CLUSTERED} SELECT var_samp(x) FROM t;")).expect("a number");
    assert!(close(answer, 1.0), "got {answer}");
}

/// Ten values one cent apart around 123456789.01. PostgreSQL 17 answers
/// 0.0008249994993211491, the closed form answered 2.0, out by a factor of
/// 2400.
#[test]
fn population_variance_over_money_matches_postgresql() {
    let rows = (0..10)
        .map(|step| format!("(123456789.01 + {step} * 0.01)"))
        .collect::<Vec<_>>()
        .join(", ");
    let answer = evaluate_one(&format!(
        "CREATE TABLE t (x DOUBLE PRECISION); INSERT INTO t VALUES {rows}; \
         SELECT var_pop(x) FROM t;"
    ))
    .expect("a number");
    assert!((answer - 0.000_824_999_499_321_149_1).abs() < 1e-9, "got {answer}");
}

/// The closed form drove this dataset's variance negative, so `sqrt` of it was
/// NULL and the standard deviation silently disappeared. PostgreSQL 17 answers
/// 0.2683650619320667 over these exact values.
#[test]
fn a_standard_deviation_over_large_offset_values_is_not_null() {
    let values: Vec<f64> = (0..50).map(|step| 1e9 + f64::from(step) / 50.0).collect();
    let rows = values.iter().map(|value| format!("({value:?})")).collect::<Vec<_>>().join(", ");
    let answer = evaluate_one(&format!(
        "CREATE TABLE t (x DOUBLE PRECISION); INSERT INTO t VALUES {rows}; \
         SELECT stddev_pop(x) FROM t;"
    ))
    .expect("a standard deviation, not NULL");
    let count = values.len() as f64;
    let mean = values.iter().sum::<f64>() / count;
    let expected =
        (values.iter().map(|value| (value - mean) * (value - mean)).sum::<f64>() / count).sqrt();
    assert!(close(answer, expected), "got {answer}, expected {expected}");
}

// ---------- the two clauses the closed form discarded ----------

const PARTITIONED: &str = "CREATE TABLE t (g INT, x DOUBLE PRECISION);
     INSERT INTO t VALUES (1, 1), (1, 3), (1, 5), (2, 10), (2, 20);";

/// PostgreSQL 17 answers one row per input row, 2.6666666666666665 three times
/// then 25 twice. The closed form dropped the whole `OVER` clause and answered
/// a single row belonging to no partition.
#[test]
fn a_window_keeps_its_partition() {
    let rows = evaluate(&format!("{PARTITIONED} SELECT var_pop(x) OVER (PARTITION BY g) FROM t;"));
    assert_eq!(rows.len(), 5, "a window answers once per row, got {rows:?}");
    let answers: Vec<f64> = rows.into_iter().map(|row| row.expect("a number")).collect();
    for answer in &answers[..3] {
        assert!(close(*answer, 2.666_666_666_666_666_5), "got {answers:?}");
    }
    for answer in &answers[3..] {
        assert!(close(*answer, 25.0), "got {answers:?}");
    }
}

/// A moving frame exercises the inverse step, which is the part of a window
/// aggregate a plain aggregate registration cannot supply.
#[test]
fn a_moving_frame_recomputes_per_row() {
    let rows = evaluate(&format!(
        "{PARTITIONED} SELECT var_pop(x) OVER (ORDER BY x ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) \
         FROM t;"
    ));
    let answers: Vec<f64> = rows.into_iter().map(|row| row.expect("a number")).collect();
    // x ordered is 1, 3, 5, 10, 20. Each two-value window has variance
    // ((b - a) / 2)^2, and the first row is alone so its variance is 0.
    assert_eq!(answers.len(), 5, "got {answers:?}");
    for (index, expected) in [0.0, 1.0, 1.0, 6.25, 25.0].into_iter().enumerate() {
        assert!(close(answers[index], expected), "row {index} of {answers:?}");
    }
}

/// PostgreSQL 17 answers 2.25 over the distinct values and 2.0 over all three
/// rows. The closed form dropped the `DISTINCT` and answered 2.0.
#[test]
fn distinct_deduplicates_before_the_variance() {
    const ROWS: &str = "CREATE TABLE t (x DOUBLE PRECISION);
         INSERT INTO t VALUES (1), (1), (4);";
    let deduplicated =
        evaluate_one(&format!("{ROWS} SELECT var_pop(DISTINCT x) FROM t;")).expect("a number");
    let every_row = evaluate_one(&format!("{ROWS} SELECT var_pop(x) FROM t;")).expect("a number");
    assert!(close(deduplicated, 2.25), "distinct: got {deduplicated}");
    assert!(close(every_row, 2.0), "all rows: got {every_row}");
}

/// `FILTER` is lowered onto `CASE` before the call is classified, so it keeps
/// working through the passthrough.
#[test]
fn a_filter_still_narrows_the_rows() {
    let answer = evaluate_one(
        "CREATE TABLE t (x DOUBLE PRECISION);
         INSERT INTO t VALUES (1), (3), (5), (-100);
         SELECT var_pop(x) FILTER (WHERE x > 0) FROM t;",
    )
    .expect("a number");
    assert!(close(answer, 2.666_666_666_666_666_5), "got {answer}");
}

// ---------- PostgreSQL's own edge cases ----------

/// Measured on PostgreSQL 17: over no rows every one of them is NULL, over one
/// row the population forms are 0 and the sample forms NULL, and `corr` is
/// NULL whenever a side does not vary.
#[test]
fn the_empty_and_single_row_cases_match_postgresql() {
    const EMPTY: &str = "CREATE TABLE t (x DOUBLE PRECISION, y DOUBLE PRECISION);";
    const ONE: &str = "CREATE TABLE t (x DOUBLE PRECISION, y DOUBLE PRECISION);
         INSERT INTO t VALUES (5, 7);";
    const FLAT: &str = "CREATE TABLE t (x DOUBLE PRECISION, y DOUBLE PRECISION);
         INSERT INTO t VALUES (5, 7), (5, 9);";

    for aggregate in ["var_pop(x)", "var_samp(x)", "stddev_pop(x)", "covar_pop(x, y)", "corr(x, y)"]
    {
        assert_eq!(
            evaluate_one(&format!("{EMPTY} SELECT {aggregate} FROM t;")),
            None,
            "{aggregate} over no rows"
        );
    }
    assert_eq!(evaluate_one(&format!("{ONE} SELECT var_pop(x) FROM t;")), Some(0.0));
    assert_eq!(evaluate_one(&format!("{ONE} SELECT stddev_pop(x) FROM t;")), Some(0.0));
    assert_eq!(evaluate_one(&format!("{ONE} SELECT covar_pop(x, y) FROM t;")), Some(0.0));
    assert_eq!(evaluate_one(&format!("{ONE} SELECT var_samp(x) FROM t;")), None);
    assert_eq!(evaluate_one(&format!("{ONE} SELECT corr(x, y) FROM t;")), None);
    assert_eq!(evaluate_one(&format!("{FLAT} SELECT corr(x, y) FROM t;")), None);
}

/// PostgreSQL skips a row whose value is NULL, and skips a pair where either
/// side is NULL. This is the property the old closed forms had to spell out
/// with `CASE` wrappers to keep the marginals over the same rows as the joint
/// term, and which the destination's own implementation now owns.
#[test]
fn one_sided_nulls_contribute_nothing() {
    const WITH_HALF_ROWS: &str = "CREATE TABLE t (x REAL, y REAL);
         INSERT INTO t VALUES (1, 2), (3, NULL), (NULL, 5), (4, 7), (2, 3);";
    const PAIRS_ONLY: &str = "CREATE TABLE t (x REAL, y REAL);
         INSERT INTO t VALUES (1, 2), (4, 7), (2, 3);";

    for aggregate in ["covar_pop(y, x)", "covar_samp(y, x)", "corr(y, x)"] {
        let with_half_rows = evaluate_one(&format!("{WITH_HALF_ROWS} SELECT {aggregate} FROM t;"))
            .expect("a number");
        let pairs_only =
            evaluate_one(&format!("{PAIRS_ONLY} SELECT {aggregate} FROM t;")).expect("a number");
        assert!(
            close(with_half_rows, pairs_only),
            "{aggregate}: {with_half_rows} with the half rows, {pairs_only} without"
        );
    }
    // The three constants PostgreSQL 17 answers over those pairs.
    assert!(close(
        evaluate_one(&format!("{WITH_HALF_ROWS} SELECT covar_pop(y, x) FROM t;"))
            .expect("a number"),
        2.666_666_666_666_666_5
    ));
    assert!(close(
        evaluate_one(&format!("{WITH_HALF_ROWS} SELECT covar_samp(y, x) FROM t;"))
            .expect("a number"),
        4.0
    ));
    assert!(close(
        evaluate_one(&format!("{WITH_HALF_ROWS} SELECT corr(y, x) FROM t;")).expect("a number"),
        0.989_743_318_610_787
    ));
}

/// A grouped query keeps one answer per group, which is the shape the closed
/// forms did get right and which must survive the passthrough.
#[test]
fn a_grouped_query_answers_once_per_group() {
    let rows = evaluate(&format!("{PARTITIONED} SELECT var_pop(x) FROM t GROUP BY g ORDER BY g;"));
    let answers: Vec<f64> = rows.into_iter().map(|row| row.expect("a number")).collect();
    assert_eq!(answers.len(), 2, "got {answers:?}");
    assert!(close(answers[0], 2.666_666_666_666_666_5), "got {answers:?}");
    assert!(close(answers[1], 25.0), "got {answers:?}");
}

/// All nine over one dataset, against the numbers PostgreSQL 17 answers for
/// it. `variance` and `stddev` are PostgreSQL's aliases for the sample forms,
/// and they now reach the destination under their own names rather than being
/// rewritten onto `var_samp` and `stddev_samp`, so the destination has to
/// carry both spellings and this proves it.
#[test]
fn every_aggregate_matches_postgresql_over_one_dataset() {
    const ROWS: &str = "CREATE TABLE m (x DOUBLE PRECISION, y DOUBLE PRECISION);
         INSERT INTO m VALUES (1, 2), (2, 4), (3, 6), (4, 8), (5, 10);";
    for (aggregate, expected) in [
        ("var_pop(x)", 2.0),
        ("var_samp(x)", 2.5),
        ("variance(x)", 2.5),
        ("stddev(x)", 1.581_138_830_084_189_8),
        ("stddev_pop(x)", 1.414_213_562_373_095_1),
        ("stddev_samp(x)", 1.581_138_830_084_189_8),
        ("covar_pop(x, y)", 4.0),
        ("covar_samp(x, y)", 5.0),
        ("corr(x, y)", 1.0),
    ] {
        let answer = evaluate_one(&format!("{ROWS} SELECT {aggregate} FROM m;")).expect("a number");
        assert!(close(answer, expected), "{aggregate}: got {answer}, expected {expected}");
    }
}

/// The reverse direction reads the emitted call back unchanged, because
/// PostgreSQL spells all nine the same way.
#[test]
fn the_emitted_call_reverses_to_itself() {
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE t (x REAL, y REAL);").expect("parse the schema");
    let schema = translator.build_schema().expect("build the schema");
    let reversed = translator
        .reverse_sql("SELECT corr(x, y) FROM t;", &schema, &declared())
        .expect("reverse translate");
    let probe = reversed.last().expect("a probe").to_string();
    assert!(probe.contains("corr(x, y)"), "{probe}");
}
