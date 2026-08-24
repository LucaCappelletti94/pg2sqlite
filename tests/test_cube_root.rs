//! `cbrt` of a negative number is negative, not NULL.
//!
//! The lowering was `pow(x, 1.0/3.0)`, and IEEE `pow` of a negative base with
//! a non-integer exponent is NaN, which SQLite answers as NULL. So
//! `cbrt(-8)` came back empty where PostgreSQL answers -2.
//!
//! Taking the root of the magnitude and putting the sign back answers both
//! halves of the domain. It is not bit-exact against PostgreSQL, whose `cbrt`
//! calls the correctly rounded C function while `pow` is not correctly
//! rounded, so a value that is not a perfect cube agrees to about fifteen
//! significant figures rather than exactly.
//!
//! Every expected value below was read off PostgreSQL 17.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};
use rusqlite::{Connection, functions::FunctionFlags, types::FromSql};

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_math_functions_available()
}

fn translate(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&options())
        .expect("translate")
        .pop()
        .expect("a probe statement")
}

/// The bundled SQLite is built without the math functions, so `pow` is
/// registered here. `abs` and `sign` are core and need nothing.
fn evaluate<T: FromSql>(pg: &str) -> T {
    let probe = translate(pg);
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    connection
        .create_scalar_function("pow", 2, FunctionFlags::SQLITE_DETERMINISTIC, |context| {
            // SQLite's own math functions answer NULL for a NULL argument.
            let (base, exponent) = (context.get::<Option<f64>>(0)?, context.get::<Option<f64>>(1)?);
            Ok(base.zip(exponent).map(|(base, exponent)| base.powf(exponent)))
        })
        .expect("register pow");
    connection
        .query_row(&probe, [], |row| row.get::<_, T>(0))
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"))
}

/// PostgreSQL answers -2. This used to be NULL.
#[test]
fn the_cube_root_of_a_negative_number_is_negative() {
    assert_eq!(evaluate::<Option<f64>>("SELECT cbrt(-8);"), Some(-2.0));
    assert_eq!(evaluate::<Option<f64>>("SELECT cbrt(-27);"), Some(-3.0));
}

#[test]
fn the_cube_root_of_a_positive_number_is_unchanged() {
    assert_eq!(evaluate::<Option<f64>>("SELECT cbrt(8);"), Some(2.0));
    assert_eq!(evaluate::<Option<f64>>("SELECT cbrt(27);"), Some(3.0));
}

#[test]
fn the_cube_root_of_zero_is_zero() {
    assert_eq!(evaluate::<Option<f64>>("SELECT cbrt(0);"), Some(0.0));
}

#[test]
fn a_null_operand_stays_null() {
    assert_eq!(evaluate::<Option<f64>>("SELECT cbrt(NULL);"), None);
}

/// PostgreSQL answers 1.2599210498948734 and -0.1. `pow` is not correctly
/// rounded, so the agreement is to about fifteen significant figures and no
/// further, which is why this is a tolerance rather than an equality.
#[test]
fn a_value_that_is_not_a_perfect_cube_matches_postgresql_closely() {
    let two: f64 = evaluate("SELECT cbrt(2);");
    assert!((two - 1.259_921_049_894_873_4).abs() < 1e-15, "got {two}");
    let small: f64 = evaluate("SELECT cbrt(-0.001);");
    assert!((small + 0.1).abs() < 1e-15, "got {small}");
}

/// Cubing the answer gets the operand back, which is the property the shape
/// exists to hold and which the old lowering broke for every negative value.
#[test]
fn cubing_the_answer_returns_the_operand() {
    for operand in [-8.0, -1.5, -0.001, 0.0, 0.001, 1.5, 8.0] {
        let root: f64 = evaluate(&format!("SELECT cbrt({operand});"));
        let cubed = root * root * root;
        assert!(
            (cubed - operand).abs() < 1e-12 * operand.abs().max(1.0),
            "cbrt({operand}) = {root}, cubed back to {cubed}"
        );
    }
}

/// The operand is read twice, once for its sign and once for its magnitude,
/// which is only observable for a volatile argument. Pinned so the shape is
/// not quietly rewritten into one that reads it more.
#[test]
fn the_emitted_shape_reads_the_operand_twice() {
    assert_eq!(translate("SELECT cbrt(-8);"), "SELECT (sign(-8) * pow(abs(-8), (1.0 / 3.0)))");
}

#[test]
fn cbrt_still_needs_the_math_functions_option() {
    let error = Pg2Sqlite::default()
        .sql("SELECT cbrt(-8);")
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("cbrt needs the math functions");
    assert!(error.to_string().contains("with_math_functions_available"), "{error}");
}

/// The emitted form is valid PostgreSQL as well, since PostgreSQL has all
/// three functions, so reading it back cannot fail even though it does not
/// recover the `cbrt` spelling.
#[test]
fn the_emitted_shape_reverses_to_valid_postgresql() {
    let translator = Pg2Sqlite::default().sql("CREATE TABLE t (x REAL);").expect("parse");
    let schema = translator.build_schema().expect("build the schema");
    let restored = translator
        .reverse_sql(&format!("{};", translate("SELECT cbrt(x) FROM t;")), &schema, &options())
        .expect("reverse translate")
        .pop()
        .expect("a statement")
        .to_string();
    assert!(restored.contains("sign(x)") && restored.contains("abs(x)"), "{restored}");
}

// ── H1: ||/ operator form of cube root returns NULL for negative operands ────

/// H1: the prefix operator ||/ emits pow(x, (1.0 / 3.0)), which returns NULL
/// for every negative operand because C pow(negative, fraction) is NaN and
/// SQLite surfaces NaN as NULL. PostgreSQL returns -2 for ||/ -8.
/// The cbrt() function form uses the sign-preserving closed form; the operator
/// arm does not, so the two forms disagree on negative inputs.
#[test]
fn the_operator_form_of_cube_root_of_negative_eight_is_negative_two() {
    assert_eq!(evaluate::<Option<f64>>("SELECT ||/ -8;"), Some(-2.0));
}

/// Same defect at a different value to pin both the translation shape and the
/// sign rule.
#[test]
fn the_operator_form_of_cube_root_of_negative_twenty_seven_is_negative_three() {
    assert_eq!(evaluate::<Option<f64>>("SELECT ||/ -27;"), Some(-3.0));
}

/// Green companion: a positive operand already works through pow(x, 1/3).
#[test]
fn the_operator_form_of_cube_root_of_positive_eight_is_two() {
    assert_eq!(evaluate::<Option<f64>>("SELECT ||/ 8;"), Some(2.0));
}

/// Green companion: NULL propagates through any form of the translation.
#[test]
fn the_operator_form_null_operand_stays_null() {
    assert_eq!(evaluate::<Option<f64>>("SELECT ||/ NULL;"), None);
}
