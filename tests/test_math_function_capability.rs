//! Focused tests for the math-function opt-in gate on `Pg2SqliteOptions`.
//!
//! Each test group covers one axis: which functions reject when the gate is
//! off, which translate when it is on, the exact emitted form for `cbrt` and
//! `power`, and that the statistical aggregates sit outside the gate
//! altogether.

mod helpers;

use diesel::connection::SimpleConnection;
use helpers::{establish_connection, translate_pg};
use pg2sqlite::prelude::Pg2SqliteOptions;

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
                m.contains("no such function:")
            } => {}
        Err(e) => panic!("SQLite rejected emitted SQL: {e}\n{sql}"),
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
