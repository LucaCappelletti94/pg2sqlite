//! What an expression over a `NUMERIC(p,s)` column answers, once the column
//! holds minor units.
//!
//! Arithmetic and comparison already honour the scaled representation. The
//! positions here did not: text, modulo, integer division, the integer and
//! float casts and one-argument rounding read the stored count instead of the
//! value, so `100.00 || 'x'` answered `10000x`. Every expected value below is
//! what PostgreSQL 17.3 prints for the same input, measured in Docker.
//!
//! The convention the answers follow, which `floor_numeric_of` states: an
//! expression whose PostgreSQL result has scale 0 is a plain integer, and one
//! that keeps a fractional part stays at the column's scale.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

/// The rows of `SELECT <expression> FROM z` over a two-column scaled table
/// holding `1.50` and `0.50`.
fn answer(expression: &str) -> Vec<Option<String>> {
    run_translated_with(
        &format!(
            "CREATE TABLE z (v numeric(10,2), w numeric(10,2));
             INSERT INTO z (v, w) VALUES (1.50, 0.50);
             SELECT {expression} FROM z;"
        ),
        &Pg2SqliteOptions::default(),
    )
}

/// The refusal `expression` earns over the same table.
fn refusal(expression: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!(
            "CREATE TABLE z (v numeric(10,2), w numeric(10,2));
             SELECT {expression} FROM z;"
        ))
        .expect("fixture parses")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("expected a refusal")
        .to_string()
}

#[test]
fn concatenation_reads_the_value_rather_than_the_stored_count() {
    // PostgreSQL: 1.50x, a1.50, 1.50x, 1.50-x, 1.500.50.
    assert_eq!(answer("v || 'x'"), vec![Some("1.50x".to_string())]);
    assert_eq!(answer("'a' || v"), vec![Some("a1.50".to_string())]);
    assert_eq!(answer("concat(v, 'x')"), vec![Some("1.50x".to_string())]);
    assert_eq!(answer("concat_ws('-', v, 'x')"), vec![Some("1.50-x".to_string())]);
    assert_eq!(answer("concat(v, w)"), vec![Some("1.500.50".to_string())]);
}

#[test]
fn concatenating_two_numerics_is_refused_as_postgresql_refuses_it() {
    // PostgreSQL: operator does not exist: numeric || numeric.
    let message = refusal("v || w");
    assert!(message.contains("||"), "{message}");
}

#[test]
fn a_negative_value_renders_with_its_sign_and_a_null_stays_null() {
    assert_eq!(
        run_translated_with(
            "CREATE TABLE z (v numeric(10,2));
             INSERT INTO z (v) VALUES (-0.50), (NULL);
             SELECT v || 'x' FROM z ORDER BY v;",
            &Pg2SqliteOptions::default(),
        ),
        // PostgreSQL orders NULL last ascending, and the emitted ORDER BY
        // carries NULLS LAST.
        vec![Some("-0.50x".to_string()), None]
    );
}

#[test]
fn the_text_cast_is_exact_past_the_double_range() {
    // PostgreSQL prints 9999999999999999.99; float division answered
    // 10000000000000000.00.
    assert_eq!(
        run_translated_with(
            "CREATE TABLE z (v numeric(18,2));
             INSERT INTO z (v) VALUES (9999999999999999.99);
             SELECT v::text FROM z;",
            &Pg2SqliteOptions::default(),
        ),
        vec![Some("9999999999999999.99".to_string())]
    );
}

#[test]
fn the_integer_cast_rounds_the_value() {
    // PostgreSQL: 2 for each spelling, rounding half away from zero.
    assert_eq!(answer("v::int"), vec![Some("2".to_string())]);
    assert_eq!(answer("v::bigint"), vec![Some("2".to_string())]);
    assert_eq!(answer("v::smallint"), vec![Some("2".to_string())]);
    assert_eq!(
        run_translated_with(
            "CREATE TABLE z (v numeric(10,2));
             INSERT INTO z (v) VALUES (-1.50);
             SELECT v::int FROM z;",
            &Pg2SqliteOptions::default(),
        ),
        vec![Some("-2".to_string())]
    );
}

#[test]
fn the_float_cast_divides_by_the_scale() {
    // PostgreSQL: 1.5.
    assert_eq!(answer("v::float8"), vec![Some("1.5".to_string())]);
    assert_eq!(answer("v::real"), vec![Some("1.5".to_string())]);
}

#[test]
fn one_argument_rounding_answers_a_whole_number() {
    // PostgreSQL: 2, at scale 0, which is what floor, ceil and trunc answer
    // here too.
    assert_eq!(answer("round(v)"), vec![Some("2".to_string())]);
    assert_eq!(answer("round(v, 0)"), vec![Some("2".to_string())]);
    assert_eq!(
        run_translated_with(
            "CREATE TABLE z (v numeric(10,2));
             INSERT INTO z (v) VALUES (-1.50);
             SELECT round(v) FROM z;",
            &Pg2SqliteOptions::default(),
        ),
        vec![Some("-2".to_string())]
    );
}

#[test]
fn rounding_to_a_negative_place_is_translated() {
    // PostgreSQL: round(123, -1) is 120, round(125, -1) is 130,
    // round(1234.56, -2) over a scaled column is 1200.
    assert_eq!(answer("round(123, -1)"), vec![Some("120".to_string())]);
    assert_eq!(answer("round(125, -1)"), vec![Some("130".to_string())]);
    assert_eq!(
        run_translated_with(
            "CREATE TABLE z (v numeric(10,2));
             INSERT INTO z (v) VALUES (1234.56);
             SELECT round(v, -2) FROM z;",
            &Pg2SqliteOptions::default(),
        ),
        vec![Some("1200".to_string())]
    );
}

#[test]
fn modulo_brings_the_plain_operand_onto_the_scale() {
    // PostgreSQL: 1.50 for both spellings against 2, and 0.50 the other way
    // round, all keeping the operand's scale.
    assert_eq!(answer("mod(v, 2)"), vec![Some("150".to_string())]);
    assert_eq!(answer("v % 2"), vec![Some("150".to_string())]);
    assert_eq!(answer("mod(2, v)"), vec![Some("50".to_string())]);
    assert_eq!(answer("2 % v"), vec![Some("50".to_string())]);
    // Two scaled operands already agreed: 0.00.
    assert_eq!(answer("v % w"), vec![Some("0".to_string())]);
}

#[test]
fn integer_division_brings_the_plain_operand_onto_the_scale() {
    // PostgreSQL: div(1.50, 2) is 0 and div(2, 1.50) is 1, both at scale 0.
    assert_eq!(answer("div(v, 2)"), vec![Some("0".to_string())]);
    assert_eq!(answer("div(2, v)"), vec![Some("1".to_string())]);
    assert_eq!(answer("div(v, w)"), vec![Some("3".to_string())]);
}

#[test]
fn the_arithmetic_that_was_already_faithful_stays_faithful() {
    // Guards the scale-preserving arms against the changes above: 2.50,
    // 1.25, 3.00, -1.50, 1.50, 1.5, 1.50, 2, and true.
    assert_eq!(answer("v + 1"), vec![Some("250".to_string())]);
    assert_eq!(answer("v - 0.25"), vec![Some("125".to_string())]);
    assert_eq!(answer("v * 2"), vec![Some("300".to_string())]);
    assert_eq!(answer("-v"), vec![Some("-150".to_string())]);
    assert_eq!(answer("abs(v)"), vec![Some("150".to_string())]);
    assert_eq!(answer("round(v, 1)"), vec![Some("150".to_string())]);
    assert_eq!(answer("least(v, 2)"), vec![Some("150".to_string())]);
    assert_eq!(answer("greatest(v, 2)"), vec![Some("200".to_string())]);
    assert_eq!(answer("floor(v)"), vec![Some("1".to_string())]);
    assert_eq!(answer("ceil(v)"), vec![Some("2".to_string())]);
    assert_eq!(answer("trunc(v)"), vec![Some("1".to_string())]);
    assert_eq!(answer("sum(v)"), vec![Some("150".to_string())]);
    assert_eq!(answer("v = 1.5"), vec![Some("1".to_string())]);
}
