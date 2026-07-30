//! Casting text to boolean.
//!
//! `CAST('true' AS INTEGER)` is 0 in SQLite, because the string does not start
//! with a digit, so mapping the cast target straight to INTEGER turns every
//! spelling PostgreSQL accepts into false.
//!
//! Measured on PostgreSQL 16: the accepted set is `t`, `tr`, `tru`, `true`,
//! `y`, `ye`, `yes`, `on`, `1` and `f`, `fa`, `fal`, `fals`, `false`, `n`,
//! `no`, `of`, `off`, `0`. Case is ignored and surrounding whitespace is
//! trimmed, so `E'\t on \n'` is true. Any unambiguous prefix works, which is
//! why `of` is accepted but `o` is not: it could be `on` or `off`. Anything
//! else raises `invalid input syntax for type boolean`.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

const TRUE_SPELLINGS: [&str; 10] = ["t", "tr", "tru", "true", "TRUE", "y", "ye", "yes", "on", "1"];
const FALSE_SPELLINGS: [&str; 11] =
    ["f", "fa", "fal", "fals", "false", "FALSE", "n", "no", "of", "off", "0"];

/// Evaluates the expression over a one row table, so the cast operand is a
/// column and cannot be folded at translation time.
fn evaluate_over_column(value: &str, expression: &str) -> Vec<Option<String>> {
    run_translated_with(
        &format!(
            "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
             INSERT INTO t VALUES (1, '{value}');
             SELECT {expression} FROM t;"
        ),
        &Pg2SqliteOptions::default(),
    )
}

#[test]
fn every_true_spelling_casts_to_one() {
    for spelling in TRUE_SPELLINGS {
        assert_eq!(
            evaluate_over_column(spelling, "CAST(s AS BOOLEAN)"),
            vec![Some("1".to_string())],
            "PostgreSQL reads `{spelling}` as true"
        );
    }
}

#[test]
fn every_false_spelling_casts_to_zero() {
    for spelling in FALSE_SPELLINGS {
        assert_eq!(
            evaluate_over_column(spelling, "CAST(s AS BOOLEAN)"),
            vec![Some("0".to_string())],
            "PostgreSQL reads `{spelling}` as false"
        );
    }
}

/// The `::boolean` spelling reaches the same place.
#[test]
fn the_cast_operator_behaves_the_same() {
    assert_eq!(evaluate_over_column("yes", "s::boolean"), vec![Some("1".to_string())]);
    assert_eq!(evaluate_over_column("off", "s::boolean"), vec![Some("0".to_string())]);
}

/// Surrounding whitespace is trimmed before the value is read.
#[test]
fn surrounding_whitespace_is_ignored() {
    assert_eq!(evaluate_over_column("  on  ", "CAST(s AS BOOLEAN)"), vec![Some("1".to_string())]);
}

/// A literal PostgreSQL will not accept is known to be wrong at translation
/// time, so it is refused then rather than answering something at runtime.
/// `o` is included because it is a prefix of both `on` and `off`.
#[test]
fn an_unreadable_literal_is_refused() {
    for spelling in ["maybe", "o", ""] {
        let error = Pg2Sqlite::default()
            .sql(&format!("SELECT CAST('{spelling}' AS BOOLEAN);"))
            .expect("parse")
            .translate(&Pg2SqliteOptions::default())
            .expect_err("PostgreSQL raises for this spelling");
        assert!(
            error.to_string().contains("boolean"),
            "the error must name the type, got: {error}"
        );
    }
}

/// A number is already a boolean in SQLite's terms, and PostgreSQL reads any
/// nonzero integer as true, so the cast must not go through the text set.
#[test]
fn casting_a_number_is_unchanged() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT);
         INSERT INTO t VALUES (1, 5);
         SELECT CAST(n AS BOOLEAN) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
}
