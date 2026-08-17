//! Focused tests for the math-function opt-in gate on `Pg2SqliteOptions`.
//!
//! Each test group covers one axis: which functions reject when the gate is
//! off, which translate when it is on, the exact emitted form for `cbrt` and
//! `power`, and that the statistical aggregates sit outside the gate
//! altogether.

mod helpers;

use diesel::connection::SimpleConnection;
use helpers::{establish_connection, translate_pg};
use pg2sqlite::{
    impls::sqlite_functions::gated_math,
    prelude::{Pg2SqliteOptions, TranslationOptions},
};

fn opts_off() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

fn opts_on() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_math_functions_available()
}

fn translate_off(pg: &str) -> Result<String, pg2sqlite::errors::Error> {
    translate_pg(pg, &opts_off()).map(|v| v.join("\n"))
}

fn translate_on(pg: &str) -> Result<String, pg2sqlite::errors::Error> {
    translate_pg(pg, &opts_on()).map(|v| v.join("\n"))
}

// ---------- option OFF: scalar math functions reject ----------

#[test]
fn off_sqrt_rejects() {
    assert!(translate_off("SELECT sqrt(2.0);").is_err());
}

#[test]
fn off_ln_rejects() {
    assert!(translate_off("SELECT ln(2.0);").is_err());
}

#[test]
fn off_exp_rejects() {
    assert!(translate_off("SELECT exp(1.0);").is_err());
}

#[test]
fn off_log_rejects() {
    assert!(translate_off("SELECT log(100.0);").is_err());
}

#[test]
fn off_log10_rejects() {
    assert!(translate_off("SELECT log10(100.0);").is_err());
}

#[test]
fn off_pow_rejects() {
    assert!(translate_off("SELECT pow(2.0, 3.0);").is_err());
}

#[test]
fn off_power_rejects() {
    assert!(translate_off("SELECT power(2.0, 3.0);").is_err());
}

#[test]
fn off_cbrt_rejects() {
    assert!(translate_off("SELECT cbrt(27.0);").is_err());
}

// ---------- option ON: scalar math functions pass through ----------

#[test]
fn on_sqrt_passes_through() {
    let sql = translate_on("SELECT sqrt(2.0);").expect("sqrt should translate when math ON");
    assert!(sql.contains("sqrt("), "{sql}");
    sqlite_parses(&sql);
}

#[test]
fn on_ln_passes_through() {
    let sql = translate_on("SELECT ln(2.0);").expect("ln should translate when math ON");
    assert!(sql.contains("ln("), "{sql}");
    sqlite_parses(&sql);
}

#[test]
fn on_exp_passes_through() {
    let sql = translate_on("SELECT exp(1.0);").expect("exp should translate when math ON");
    assert!(sql.contains("exp("), "{sql}");
    sqlite_parses(&sql);
}

#[test]
fn on_log_passes_through() {
    let sql = translate_on("SELECT log(100.0);").expect("log should translate when math ON");
    assert!(sql.contains("log("), "{sql}");
    sqlite_parses(&sql);
}

#[test]
fn on_log10_passes_through() {
    let sql = translate_on("SELECT log10(100.0);").expect("log10 should translate when math ON");
    assert!(sql.contains("log10("), "{sql}");
    sqlite_parses(&sql);
}

// ---------- option ON: power renamed, cbrt translated ----------

#[test]
fn on_power_renames_to_pow() {
    let sql = translate_on("SELECT power(2.0, 3.0);").expect("power should translate when math ON");
    assert!(sql.contains("pow(2.0, 3.0)"), "expected pow(2.0, 3.0) in: {sql}");
    assert!(!sql.contains("power("), "should not contain power(: {sql}");
    sqlite_parses(&sql);
}

/// The sign is carried outside the power, because a negative base under a
/// non-integer exponent is NaN. `tests/test_cube_root.rs` holds the numbers.
#[test]
fn on_cbrt_roots_the_magnitude_and_restores_the_sign() {
    let sql = translate_on("SELECT cbrt(27.0);").expect("cbrt should translate when math ON");
    assert!(
        sql.contains("(sign(27.0) * pow(abs(27.0), (1.0 / 3.0)))"),
        "expected the signed cube root in: {sql}"
    );
    sqlite_parses(&sql);
}

/// Runs the emitted SQL against an in-memory SQLite with a fixture table.
///
/// The bundled build is 3.49.1 without `SQLITE_ENABLE_MATH_FUNCTIONS`, measured
/// rather than assumed, so a gated name resolves to nothing there. That precise
/// error is accepted as proof SQLite parsed the rest of the statement, and no
/// other error is.
fn sqlite_parses(sql: &str) {
    let mut conn = establish_connection();
    conn.batch_execute("CREATE TABLE t (v REAL, a REAL, b REAL, n REAL) STRICT;").unwrap();
    match conn.batch_execute(sql) {
        Ok(()) => {}
        Err(e)
            if {
                let m = e.to_string();
                gated_math().iter().any(|f| m.contains(&format!("no such function: {f}")))
            } => {}
        Err(e) => panic!("SQLite rejected emitted SQL: {e}\n{sql}"),
    }
}

/// A call shape that parses for `name`, since arity is not what these tests are
/// about.
fn call(name: &str) -> String {
    match name {
        "pi" => format!("{name}()"),
        "atan2" | "log" | "mod" | "pow" | "power" | "trunc" => format!("{name}(a, b)"),
        _ => format!("{name}(v)"),
    }
}

/// The option says the destination carries SQLite's maths build, so every name
/// that build answers may be emitted.
///
/// It used to admit only the names with a translation arm of their own, which
/// left the whole trigonometry family refused while `sqrt` passed: `acos`,
/// `acosh`, `asin`, `asinh`, `atan`, `atan2`, `atanh`, `ceiling`, `cos`,
/// `cosh`, `degrees`, `log2`, `pi`, `radians`, `sin`, `sinh`, `tan` and `tanh`,
/// all eighteen of them names both engines answer, `log2` excepted. The reverse
/// direction accepts every one, so the refusal was the same omission as the
/// aggregate one, pointing the other way.
#[test]
fn the_option_admits_every_name_the_maths_build_answers() {
    for name in gated_math() {
        let query = format!("SELECT {} FROM t;", call(name));
        let emitted = translate_on(&query)
            .unwrap_or_else(|error| panic!("{name} is in the maths build: {error}"));
        sqlite_parses(&emitted);
    }
}

/// The other half of the gate, which the widening must not undo: with no
/// declaration, a gated name is never emitted. A few are not refused at all
/// because they are lowered to something portable instead, `ceil` and `floor`
/// into `CASE` over `CAST` and `mod` into the `%` operator, so the assertion is
/// about what comes out rather than about failing.
#[test]
fn without_the_option_no_gated_name_is_emitted() {
    for name in gated_math() {
        let query = format!("SELECT {} FROM t;", call(name));
        match translate_off(&query) {
            Ok(emitted) => {
                assert!(
                    !emitted.to_lowercase().contains(&format!("{name}(")),
                    "{name} was emitted without the maths declaration: {emitted}"
                );
            }
            // The refusal has to point at the build, not send the caller off to
            // register a function the build would already carry. Fourteen of
            // these used to give that wrong advice, having no arm of their own.
            // The flag rather than the option name, because `trunc` carries its
            // own narrower message about a computed scale needing `pow`.
            Err(error) => {
                let message = error.to_string();
                assert!(
                    message.contains("SQLITE_ENABLE_MATH_FUNCTIONS"),
                    "the refusal for {name} should name the build, got: {message}"
                );
            }
        }
    }
}

/// `sign` is not one of the gated names, measured against the bundled build:
/// it answers there with no maths flag, and this crate's own SQLite inventory
/// lists it unconditionally. It was refused all the same, because an arm naming
/// it sat in front of the passthrough the inventory would have given it.
#[test]
fn sign_needs_no_declaration_because_sqlite_always_has_it() {
    let emitted = translate_off("SELECT sign(v) FROM t;").expect("SQLite answers sign unaided");
    assert_eq!(emitted, "SELECT sign(v) FROM t");
    sqlite_parses(&emitted);
}

// ---------- the statistical aggregates are outside this gate ----------

/// The nine statistical aggregates used to depend on this option, four of them
/// because their closed form called `sqrt`. The closed forms are gone, so the
/// option now decides nothing for them in either direction: undeclared they
/// are refused with it on, and declared they translate with it off.
#[test]
fn the_statistical_aggregates_no_longer_consult_the_math_option() {
    for (name, arguments) in [
        ("var_pop", "v"),
        ("var_samp", "v"),
        ("variance", "v"),
        ("stddev", "v"),
        ("stddev_pop", "v"),
        ("stddev_samp", "v"),
        ("covar_pop", "a, b"),
        ("covar_samp", "a, b"),
        ("corr", "a, b"),
    ] {
        let query = format!("SELECT {name}({arguments}) FROM t;");
        assert!(
            translate_on(&query).is_err(),
            "{name} must stay refused when only the math option is on"
        );
        let declared = Pg2SqliteOptions::default().with_user_defined_functions([name]);
        let emitted = translate_pg(&query, &declared)
            .unwrap_or_else(|error| panic!("{name} must translate once declared: {error}"))
            .join("\n");
        assert_eq!(emitted, format!("SELECT {name}({arguments}) FROM t"), "{name}");
    }
}
