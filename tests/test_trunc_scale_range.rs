//! F35: the scales `trunc(x, n)` can actually be translated for.
//!
//! The translation multiplies by `10^n`, cuts to a whole number, and divides
//! back. That works only while the multiplied value still fits a 64-bit whole
//! number, and while `10^n` is a number a double can hold at all. Outside those
//! two bounds it used to answer wrongly in silence, and at a large enough scale
//! it built the factor one character at a time into a four gigabyte statement.
//!
//! Both bounds are properties of the destination, measured: `1e-323` is a
//! nonzero double and `1e-324` is zero, and `trunc(1.5, 18)` answers 1.5 while
//! `trunc(1.5, 19)` answers the saturated 64-bit maximum divided by the factor.
//!
//! Every expectation below was read off PostgreSQL 17 first. The tests run the
//! emitted SQL, because the whole defect was output that parses and answers the
//! wrong thing.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

const FIXTURE: &str = "CREATE TABLE t (id INT PRIMARY KEY);";

fn translate(dml: &str) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(&format!("{FIXTURE}\n{dml}"))
        .map_err(|e| e.to_string())?
        .translate_to_sql(&Pg2SqliteOptions::default())
        .map_err(|e| e.to_string())
}

/// Runs the emitted script and returns the last statement's first column.
fn answer(dml: &str) -> String {
    let statements = translate(dml).expect("translate");
    let probe = statements.last().expect("a statement").clone();
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements[..statements.len() - 1] {
        connection.execute_batch(&format!("{statement};")).expect("emitted setup");
    }
    connection.execute("INSERT INTO t (id) VALUES (1)", []).expect("row");
    connection
        .query_row(&format!("SELECT CAST(({probe}) AS TEXT)"), [], |r| {
            r.get::<_, Option<String>>(0)
        })
        .unwrap_or_else(|error| panic!("emitted probe failed: {probe}: {error}"))
        .unwrap_or_else(|| panic!("emitted probe answered NULL: {probe}"))
}

fn refusal(dml: &str) -> String {
    translate(dml).expect_err("this scale cannot be translated")
}

// --- the negative side, which is exactly fixable ---------------------------

/// PostgreSQL answers 0. The factor used to round to a literal zero and the
/// emitted statement divided by it, so the answer was NULL.
#[test]
fn a_scale_past_ten_decimal_places_answers_zero_not_null() {
    assert_eq!(answer("SELECT trunc(12345.6, -11) FROM t;"), "0.0");
}

#[test]
fn a_far_negative_scale_answers_zero() {
    assert_eq!(answer("SELECT trunc(12345.6, -30) FROM t;"), "0.0");
}

#[test]
fn the_negative_scales_that_worked_still_work() {
    assert_eq!(answer("SELECT trunc(12345.6, -2) FROM t;"), "12300.0");
    assert_eq!(answer("SELECT trunc(12345.6, -10) FROM t;"), "0.0");
}

/// `1e-323` is a nonzero double, so the fold still reaches it.
#[test]
fn the_last_representable_negative_scale_still_folds() {
    let emitted = translate("SELECT trunc(12345.6, -323) FROM t;").expect("translate");
    assert!(!emitted.last().expect("a statement").contains("pow("), "{emitted:?}");
}

/// `1e-324` is zero as a double, so the shape would divide by zero.
#[test]
fn a_scale_below_the_smallest_double_is_refused() {
    let error = refusal("SELECT trunc(12345.6, -324) FROM t;");
    assert!(error.contains("-324"), "{error}");
}

// --- the positive side, which has no fix -----------------------------------

#[test]
fn the_positive_scales_that_worked_still_work() {
    assert_eq!(answer("SELECT trunc(12345.6, 2) FROM t;"), "12345.6");
    assert_eq!(answer("SELECT trunc(1.5, 18) FROM t;"), "1.5");
}

/// Past 18 the cast cannot hold the scaled value for an operand of magnitude
/// one or more, so it saturated and the answer was silently wrong.
#[test]
fn a_scale_the_cast_cannot_hold_is_refused() {
    let error = refusal("SELECT trunc(12345.6, 19) FROM t;");
    assert!(error.contains("19"), "{error}");
}

#[test]
fn the_scale_that_used_to_answer_a_fraction_is_refused() {
    let error = refusal("SELECT trunc(12345.6, 20) FROM t;");
    assert!(error.contains("20"), "{error}");
}

/// The one in the finding's title. It used to translate, successfully, into a
/// four gigabyte statement.
#[test]
fn the_runaway_scale_is_refused_rather_than_built() {
    let error = refusal("SELECT trunc(12345.6, 2000000000) FROM t;");
    assert!(error.contains("2000000000"), "{error}");
}

/// An out-of-range literal refuses even where the computed path is open,
/// because `pow(10, 400)` is infinity and reaches the same wrong answer.
#[test]
fn an_out_of_range_scale_does_not_fall_through_to_pow() {
    let error = Pg2Sqlite::default()
        .sql(&format!("{FIXTURE}\nSELECT trunc(12345.6, 400) FROM t;"))
        .expect("parse")
        .translate_to_sql(
            &<Pg2SqliteOptions as pg2sqlite::prelude::TranslationOptions>::with_math_functions_available(
                Pg2SqliteOptions::default(),
            ),
        )
        .expect_err("an out-of-range literal scale is refused whatever is available")
        .to_string();
    assert!(error.contains("400"), "{error}");
}

/// A scale that is not a literal keeps its old behaviour, since nothing is
/// known about it at translation time.
#[test]
fn a_computed_scale_is_untouched() {
    let error = refusal("SELECT trunc(12345.6, id) FROM t;");
    assert!(error.contains("pow()"), "{error}");
}
