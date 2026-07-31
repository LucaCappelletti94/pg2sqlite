//! `covar_pop`, `covar_samp`, and `corr` over data with one-sided NULLs.
//!
//! PostgreSQL takes all three over the rows where BOTH arguments are non-NULL.
//! The closed forms took each marginal over its own row set, so a row with one
//! NULL still moved `avg(x)` while contributing nothing to `avg(x*y)`.
//!
//! Measured on PostgreSQL 16 over `(1,2)`, `(3,NULL)`, `(NULL,5)`, `(4,7)`,
//! `(2,3)`, where the three complete pairs are `(1,2)`, `(4,7)`, and `(2,3)`:
//! `covar_pop(y, x)` is 2.6666666666666665, `covar_samp(y, x)` is 4, and
//! `corr(y, x)` is 0.989743318610787. Deleting the two half rows leaves those
//! answers unchanged, which is the property under test.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};
use rusqlite::{Connection, functions::FunctionFlags};

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, x REAL, y REAL);
     INSERT INTO t VALUES (1, 1.0, 2.0), (2, 3.0, NULL), (3, NULL, 5.0),
                          (4, 4.0, 7.0), (5, 2.0, 3.0);";

/// The same rows with the two half rows removed, which is what PostgreSQL
/// aggregates over.
const PAIRS_ONLY: &str = "CREATE TABLE t (id INT PRIMARY KEY, x REAL, y REAL);
     INSERT INTO t VALUES (1, 1.0, 2.0), (4, 4.0, 7.0), (5, 2.0, 3.0);";

/// Runs the translated statements and returns the last one's single value.
///
/// This does not use the shared `run_translated_with` helper because `corr`
/// divides by two square roots and the bundled SQLite is built without
/// `SQLITE_ENABLE_MATH_FUNCTIONS`, so `sqrt` is registered here. Registering it
/// rather than skipping `corr` is what keeps the assertion numeric.
fn evaluate(table: &str, expression: &str) -> f64 {
    let options = Pg2SqliteOptions::default().with_math_functions_available();
    let mut statements = Pg2Sqlite::default()
        .sql(&format!("{table} SELECT {expression} FROM t;"))
        .expect("parse")
        .translate_to_sql(&options)
        .expect("translate");
    let probe = statements.pop().expect("a probe");

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    connection
        .create_scalar_function("sqrt", 1, FunctionFlags::SQLITE_DETERMINISTIC, |context| {
            Ok(context.get::<f64>(0)?.sqrt())
        })
        .expect("register sqrt");
    for statement in &statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }
    connection
        .query_row(&probe, [], |row| row.get::<_, f64>(0))
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"))
}

#[test]
fn covar_pop_ignores_one_sided_nulls() {
    let answer = evaluate(TABLE, "covar_pop(y, x)");
    assert!((answer - 2.666_666_666_666_666_5).abs() < 1e-12, "got {answer}");
}

#[test]
fn covar_samp_ignores_one_sided_nulls() {
    let answer = evaluate(TABLE, "covar_samp(y, x)");
    assert!((answer - 4.0).abs() < 1e-12, "got {answer}");
}

/// `corr` divides by the two marginal deviations, so it is only right when
/// those are taken over the paired rows as well.
#[test]
fn corr_ignores_one_sided_nulls() {
    let answer = evaluate(TABLE, "corr(y, x)");
    assert!((answer - 0.989_743_318_610_787).abs() < 1e-12, "got {answer}");
}

/// Dropping the rows PostgreSQL ignores must not change any of the three. This
/// states the property directly rather than restating the three constants, so
/// it still holds if the fixture changes.
#[test]
fn the_half_rows_contribute_nothing() {
    for aggregate in ["covar_pop(y, x)", "covar_samp(y, x)", "corr(y, x)"] {
        let with_half_rows = evaluate(TABLE, aggregate);
        let pairs_only = evaluate(PAIRS_ONLY, aggregate);
        assert!(
            (with_half_rows - pairs_only).abs() < 1e-12,
            "{aggregate}: {with_half_rows} with the half rows, {pairs_only} without"
        );
    }
}
