//! F27: two readers of an integer literal, disagreeing about `+5`.
//!
//! `function.rs` had a private one returning `i32` with no unary-plus arm, and
//! `array.rs` a `pub(crate)` one returning `i64` that handled it. Same job,
//! different answers, so a scale written `+2` folded in the array paths and
//! not in the arithmetic ones: `trunc(r, 2)` produced a literal factor while
//! `trunc(r, +2)` refused, and `round(amount, +1)` over a NUMERIC refused
//! outright where `round(amount, 1)` translated.
//!
//! One reader now serves both. The tests execute the emitted SQL, since a
//! folded factor that does not run is worth nothing, and check that the two
//! spellings produce the same answer rather than merely both producing one.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

const FIXTURE: &str = "CREATE TABLE t (id INT PRIMARY KEY, r REAL, amount NUMERIC(10,2));";

fn translate(dml: &str) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(&format!("{FIXTURE}\n{dml}"))
        .map_err(|e| e.to_string())?
        .translate_to_sql(&Pg2SqliteOptions::default())
        .map_err(|e| e.to_string())
}

/// Runs the emitted script over one row and returns the last statement's first
/// column, rendered the way SQLite renders it.
fn answer(dml: &str, row: &str) -> String {
    let statements = translate(dml).expect("translate");
    let probe = statements.last().expect("a statement").clone();
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements[..statements.len() - 1] {
        connection.execute_batch(&format!("{statement};")).expect("emitted setup");
    }
    connection.execute_batch(row).expect("row");
    connection
        .query_row(&format!("SELECT CAST(({probe}) AS TEXT)"), [], |r| {
            r.get::<_, Option<String>>(0)
        })
        .unwrap_or_else(|error| panic!("emitted probe failed: {probe}: {error}"))
        .unwrap_or_else(|| panic!("emitted probe answered NULL: {probe}"))
}

const ROW: &str = "INSERT INTO t (id, r, amount) VALUES (1, 1.987, 1234);";

#[test]
fn a_signed_scale_folds_like_an_unsigned_one_in_trunc() {
    let plain = translate("SELECT trunc(r, 2) FROM t;").expect("translate");
    let signed = translate("SELECT trunc(r, +2) FROM t;").expect("the signed scale folds too");
    assert_eq!(plain.last(), signed.last(), "the two spellings mean the same thing");
    assert!(
        !signed.last().expect("a statement").contains("pow("),
        "a literal scale keeps pow out of the emitted SQL: {signed:?}"
    );
}

#[test]
fn a_signed_scale_folds_like_an_unsigned_one_in_numeric_round() {
    let plain = translate("SELECT round(amount, 1) FROM t;").expect("translate");
    let signed = translate("SELECT round(amount, +1) FROM t;").expect("the signed scale folds too");
    assert_eq!(plain.last(), signed.last());
}

/// The two spellings have to answer the same thing, not merely translate.
#[test]
fn both_spellings_answer_the_same_for_trunc() {
    assert_eq!(
        answer("SELECT trunc(r, 2) FROM t;", ROW),
        answer("SELECT trunc(r, +2) FROM t;", ROW)
    );
}

#[test]
fn both_spellings_answer_the_same_for_numeric_round() {
    assert_eq!(
        answer("SELECT round(amount, 1) FROM t;", ROW),
        answer("SELECT round(amount, +1) FROM t;", ROW)
    );
}

/// A negative scale keeps working, which the shared reader spells with
/// `wrapping_neg` where the deleted one used plain negation.
#[test]
fn a_negative_scale_still_folds() {
    let emitted = translate("SELECT trunc(r, -2) FROM t;").expect("translate");
    assert!(!emitted.last().expect("a statement").contains("pow("), "{emitted:?}");
}

/// A scale that is not a literal at all still takes the computed path, which
/// without math functions is a refusal.
#[test]
fn a_computed_scale_is_still_refused_without_math_functions() {
    let error = translate("SELECT trunc(r, id) FROM t;").expect_err("a computed scale needs pow");
    assert!(error.contains("pow()"), "{error}");
}

/// The refusal the merge touched had its own text mangled by a wrapped string.
#[test]
fn the_numeric_round_refusal_reads_as_one_sentence() {
    let error = translate("SELECT round(amount, id) FROM t;").expect_err("not a literal");
    assert!(!error.contains("  "), "no run of spaces from a wrapped literal: {error}");
    assert!(error.contains("literal"), "{error}");
}
