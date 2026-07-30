//! `trunc(x, n)` truncates toward zero where SQLite's `round` rounds half away
//! from it.
//!
//! Measured on PostgreSQL 16: `trunc(1.7, 0)` is 1, `trunc(-1.7, 0)` is -1,
//! `trunc(1.789, 2)` is 1.78, `trunc(2.5, 0)` is 2, `trunc(123.456, -1)` is
//! 120. The rounding translation answered 2 for the first of those.
//!
//! The last four rows below are the ones a scale-and-truncate rewrite gets
//! wrong on its own: PostgreSQL computes in exact decimal, so `trunc(1.15, 2)`
//! is 1.15, while a double holds 1.15 as slightly less than that and
//! `1.15 * 100` is 114.99999999999999, which truncates to 1.14.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::Pg2SqliteOptions;
use run_translated_helper::run_translated_with;

/// Each row is the PostgreSQL 16 answer for `trunc(x, n)`.
const CASES: [(&str, i32, f64); 12] = [
    ("1.7", 0, 1.0),
    ("-1.7", 0, -1.0),
    ("2.5", 0, 2.0),
    ("-0.4", 0, 0.0),
    ("1.789", 2, 1.78),
    ("-1.789", 2, -1.78),
    ("100.55", 1, 100.5),
    ("123.456", -1, 120.0),
    ("1.15", 2, 1.15),
    ("2.675", 2, 2.67),
    ("0.29", 2, 0.29),
    ("1.005", 2, 1.00),
];

fn evaluate(expression: &str) -> Option<String> {
    run_translated_with(
        &format!("CREATE TABLE t (id INT PRIMARY KEY); INSERT INTO t VALUES (1); SELECT {expression} FROM t;"),
        &Pg2SqliteOptions::default(),
    )
    .into_iter()
    .next()
    .expect("one row")
}

#[test]
fn trunc_truncates_toward_zero() {
    for (x, n, expected) in CASES {
        let answer = evaluate(&format!("trunc({x}, {n})")).expect("a value");
        let answer: f64 = answer.parse().expect("a number");
        assert!(
            (answer - expected).abs() < 1e-12,
            "trunc({x}, {n}) is {expected} in PostgreSQL, got {answer}"
        );
    }
}

/// The single-argument form was already correct and must stay an integer.
#[test]
fn single_argument_trunc_still_casts() {
    assert_eq!(evaluate("trunc(3.7)"), Some("3".to_string()));
    assert_eq!(evaluate("trunc(-3.7)"), Some("-3".to_string()));
}

/// A non-literal scale needs `pow`, which ships only under
/// `SQLITE_ENABLE_MATH_FUNCTIONS`, so it is refused rather than emitted into a
/// build that would answer `no such function`.
#[test]
fn a_computed_scale_is_refused_without_math_functions() {
    let error = pg2sqlite::prelude::Pg2Sqlite::default()
        .sql("CREATE TABLE t (id INT PRIMARY KEY, n INT); SELECT trunc(1.7, n) FROM t;")
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("pow is not available by default");
    assert!(error.to_string().contains("math"), "the error must point at the option, got: {error}");
}
