//! Focused tests for the math-function opt-in gate on `Pg2SqliteOptions`.
//!
//! Each test group covers one axis: which functions reject when the gate is
//! off, which translate when it is on, the exact emitted form for `cbrt` and
//! `power`, and that non-sqrt aggregates are unaffected by the option. One
//! test executes the `stddev_pop` closed form against a real SQLite connection
//! with a registered `sqrt` UDF and checks the numeric result.

mod helpers;

use diesel::{
    Insertable, QueryableByName, connection::SimpleConnection, insert_into, prelude::*, sql_query,
    sql_types::Double, table,
};
use helpers::{establish_connection, translate_pg};
use pg2sqlite::prelude::{Pg2SqliteOptions, TranslationOptions};

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

// ---------- option OFF: sqrt-dependent aggregates reject ----------

#[test]
fn off_stddev_pop_rejects_with_sqrt_message() {
    let err = translate_off("SELECT stddev_pop(n) FROM t;").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("sqrt"), "expected sqrt mention in: {msg}");
    assert!(msg.contains("with_math_functions_available"), "expected opt-in name in: {msg}");
}

#[test]
fn off_stddev_samp_rejects() {
    assert!(translate_off("SELECT stddev_samp(n) FROM t;").is_err());
}

#[test]
fn off_stddev_rejects() {
    assert!(translate_off("SELECT stddev(n) FROM t;").is_err());
}

#[test]
fn off_corr_rejects() {
    assert!(translate_off("SELECT corr(n, n) FROM t;").is_err());
}

// ---------- option OFF: non-sqrt aggregates still translate ----------

#[test]
fn off_var_pop_still_works() {
    let sql = translate_off("SELECT var_pop(v) FROM t;").expect("var_pop should translate");
    assert!(sql.contains("avg(v * v)"), "{sql}");
}

#[test]
fn off_var_samp_still_works() {
    let sql = translate_off("SELECT var_samp(v) FROM t;").expect("var_samp should translate");
    assert!(sql.contains("sum(v * v)") && sql.contains("count(v) - 1"), "{sql}");
}

#[test]
fn off_variance_still_works() {
    let sql = translate_off("SELECT variance(v) FROM t;").expect("variance should translate");
    assert!(sql.contains("sum(v * v)"), "{sql}");
}

/// Both covariances are closed forms over `sum`, `avg`, and `count`, so they
/// translate with the math functions off. The assertion is the absence of
/// `sqrt`, not the shape of the arithmetic, which R41 changed when it made the
/// marginals ignore rows whose partner is NULL.
#[test]
fn off_covar_pop_still_works() {
    let sql = translate_off("SELECT covar_pop(a, b) FROM t;").expect("covar_pop should translate");
    assert!(!sql.contains("sqrt"), "covar_pop must need no math function: {sql}");
}

#[test]
fn off_covar_samp_still_works() {
    let sql =
        translate_off("SELECT covar_samp(a, b) FROM t;").expect("covar_samp should translate");
    assert!(!sql.contains("sqrt"), "covar_samp must need no math function: {sql}");
}

// ---------- option ON: scalar math functions pass through ----------

#[test]
fn on_sqrt_passes_through() {
    let sql = translate_on("SELECT sqrt(2.0);").expect("sqrt should translate when math ON");
    assert!(sql.contains("sqrt("), "{sql}");
}

#[test]
fn on_ln_passes_through() {
    let sql = translate_on("SELECT ln(2.0);").expect("ln should translate when math ON");
    assert!(sql.contains("ln("), "{sql}");
}

#[test]
fn on_exp_passes_through() {
    let sql = translate_on("SELECT exp(1.0);").expect("exp should translate when math ON");
    assert!(sql.contains("exp("), "{sql}");
}

#[test]
fn on_log_passes_through() {
    let sql = translate_on("SELECT log(100.0);").expect("log should translate when math ON");
    assert!(sql.contains("log("), "{sql}");
}

#[test]
fn on_log10_passes_through() {
    let sql = translate_on("SELECT log10(100.0);").expect("log10 should translate when math ON");
    assert!(sql.contains("log10("), "{sql}");
}

// ---------- option ON: power renamed, cbrt translated ----------

#[test]
fn on_power_renames_to_pow() {
    let sql = translate_on("SELECT power(2.0, 3.0);").expect("power should translate when math ON");
    assert!(sql.contains("pow(2.0, 3.0)"), "expected pow(2.0, 3.0) in: {sql}");
    assert!(!sql.contains("power("), "should not contain power(: {sql}");
}

#[test]
fn on_cbrt_translates_to_pow_one_third() {
    let sql = translate_on("SELECT cbrt(27.0);").expect("cbrt should translate when math ON");
    assert!(sql.contains("pow(27.0, (1.0 / 3.0))"), "expected pow(27.0, (1.0 / 3.0)) in: {sql}");
}

// ---------- option ON: sqrt-dependent aggregates translate ----------

#[test]
fn on_stddev_pop_translates() {
    let sql = translate_on("SELECT stddev_pop(v) FROM t;")
        .expect("stddev_pop should translate when math ON");
    assert!(sql.contains("sqrt("), "{sql}");
}

#[test]
fn on_stddev_translates() {
    let sql =
        translate_on("SELECT stddev(v) FROM t;").expect("stddev should translate when math ON");
    assert!(sql.contains("sqrt("), "{sql}");
}

#[test]
fn on_corr_translates() {
    let sql =
        translate_on("SELECT corr(a, b) FROM t;").expect("corr should translate when math ON");
    assert!(sql.contains("sqrt("), "{sql}");
}

// ---------- var_samp unaffected by the option ----------

#[test]
fn var_samp_unaffected_by_math_option() {
    let off = translate_off("SELECT var_samp(v) FROM t;").expect("var_samp off");
    let on = translate_on("SELECT var_samp(v) FROM t;").expect("var_samp on");
    assert_eq!(off, on, "var_samp output must not depend on math option");
}

// ---------- numeric correctness: stddev_pop via diesel with sqrt UDF
// ----------

table! {
    /// Numeric data for the math-capability correctness test.
    mcap (id) {
        /// Row identifier.
        id -> Integer,
        /// Value column the aggregate runs over.
        v -> Double,
    }
}

/// Insertable row for the numeric correctness test.
#[derive(Insertable)]
#[diesel(table_name = mcap)]
struct McapRow {
    id: i32,
    v: f64,
}

/// Scalar aggregate result bound to the `r` alias.
#[derive(QueryableByName)]
struct Scalar {
    #[diesel(sql_type = diesel::sql_types::Double)]
    r: f64,
}

/// Verifies that the closed-form `stddev_pop` translation is numerically
/// correct when executed against a real SQLite connection that has `sqrt`
/// registered as a UDF.
///
/// Data: v in 1..=5. var_pop = 2, so stddev_pop = sqrt(2).
///
/// `sql_query` is justified here: the SQL under test is the dynamic
/// translated output of pg2sqlite, not a statically known query.
#[test]
fn stddev_pop_closed_form_gives_correct_numeric_result() {
    let mut conn = establish_connection();
    conn.register_sql_function::<Double, Double, _, _, _>("sqrt", true, |x: f64| x.sqrt())
        .expect("register sqrt");

    let ddl = translate_on("CREATE TABLE mcap (id INTEGER PRIMARY KEY, v REAL NOT NULL);")
        .expect("DDL translates");
    conn.batch_execute(&ddl).expect("create table");

    let rows: Vec<McapRow> = (1..=5).map(|i| McapRow { id: i, v: f64::from(i) }).collect();
    insert_into(mcap::table).values(&rows).execute(&mut conn).expect("seed");

    let translated = translate_on("SELECT stddev_pop(v) AS r FROM mcap;").expect("translate");
    let result: f64 = sql_query(&translated).get_result::<Scalar>(&mut conn).expect("execute").r;

    assert!((result - 2.0_f64.sqrt()).abs() < 1e-9, "stddev_pop should be sqrt(2), got {result}");
}
