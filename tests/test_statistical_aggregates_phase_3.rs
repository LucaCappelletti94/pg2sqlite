//! Red tests for bivariate statistical aggregates.
//!
//! Phase 3: `covar_pop(x, y)` becomes `avg(x*y) - avg(x)*avg(y)`,
//! `covar_samp` becomes the sample form, `corr(x, y)` combines
//! covariance with the stddev_pop of each side.

mod helpers;

use diesel::{
    Insertable, QueryableByName, connection::SimpleConnection, insert_into, prelude::*, sql_query,
    sql_types::Double, sqlite::SqliteConnection, table,
};
use helpers::{establish_connection, translate_pg};
use pg2sqlite::prelude::{Pg2SqliteOptions, TranslationOptions};

table! {
    /// Bivariate test data for covariance and correlation aggregates.
    m (id) {
        /// Row identifier.
        id -> Integer,
        /// First numeric column.
        x -> Double,
        /// Second numeric column.
        y -> Double,
    }
}

/// Insertable row used to seed the bivariate test data.
#[derive(Insertable)]
#[diesel(table_name = m)]
struct NewRow {
    /// Row identifier.
    id: i32,
    /// First numeric value.
    x: f64,
    /// Second numeric value.
    y: f64,
}

/// Scalar aggregate result, bound by the `r` alias in each test query.
#[derive(QueryableByName)]
struct Scalar {
    /// Aggregate value.
    #[diesel(sql_type = Double)]
    r: f64,
}

fn translate(pg: &str) -> String {
    translate_pg(pg, &Pg2SqliteOptions::default().with_math_functions_available())
        .expect("translation failed")
        .join("\n")
}

/// y = 2*x for x in 1..=5. covar_pop = 4, covar_samp = 5, corr = 1
/// (perfectly correlated).
fn seed(conn: &mut SqliteConnection) {
    conn.batch_execute(&translate(
        "CREATE TABLE m (id INTEGER PRIMARY KEY, x REAL NOT NULL, y REAL NOT NULL);",
    ))
    .expect("apply schema");
    let rows: Vec<NewRow> =
        (1..=5).map(|i| NewRow { id: i, x: f64::from(i), y: 2.0 * f64::from(i) }).collect();
    insert_into(m::table).values(&rows).execute(conn).expect("seed");
}

fn aggregate(conn: &mut SqliteConnection, pg_sql: &str) -> f64 {
    sql_query(translate(pg_sql)).get_result::<Scalar>(conn).expect("aggregate").r
}

fn open_with_sqrt() -> SqliteConnection {
    let mut conn = establish_connection();
    conn.register_sql_function::<Double, Double, _, _, _>("sqrt", true, |x: f64| x.sqrt())
        .expect("register sqrt");
    conn
}

// Phase 3: closed-form rewrites

/// `corr` is the only one of the three that needs a math function, which is
/// what decides whether it can be emitted at all. The other two are proved over
/// a known dataset by the `p3_apply_*` tests below.
#[test]
fn p3_corr_needs_sqrt_and_the_covariances_do_not() {
    assert!(translate("SELECT corr(x, y) FROM m;").contains("sqrt("));
    assert!(!translate("SELECT covar_pop(x, y) FROM m;").contains("sqrt("));
    assert!(!translate("SELECT covar_samp(x, y) FROM m;").contains("sqrt("));
}

#[test]
fn p3_apply_covar_pop_known_dataset() {
    let mut conn = establish_connection();
    seed(&mut conn);
    let r = aggregate(&mut conn, "SELECT covar_pop(x, y) AS r FROM m;");
    assert!((r - 4.0).abs() < 1e-9, "covar_pop should be 4.0, got {r}");
}

#[test]
fn p3_apply_covar_samp_known_dataset() {
    let mut conn = establish_connection();
    seed(&mut conn);
    let r = aggregate(&mut conn, "SELECT covar_samp(x, y) AS r FROM m;");
    assert!((r - 5.0).abs() < 1e-9, "covar_samp should be 5.0, got {r}");
}

#[test]
fn p3_apply_corr_perfectly_correlated() {
    let mut conn = open_with_sqrt();
    seed(&mut conn);
    let r = aggregate(&mut conn, "SELECT corr(x, y) AS r FROM m;");
    assert!((r - 1.0).abs() < 1e-9, "corr should be 1.0 (perfectly linear), got {r}");
}

// Cover the two arity-check branches in `two_aggregate_args`.

#[test]
fn corr_with_no_args_errors() {
    assert!(translate_pg("SELECT corr() FROM m;", &Pg2SqliteOptions::default()).is_err());
}

#[test]
fn corr_with_one_arg_errors() {
    assert!(translate_pg("SELECT corr(x) FROM m;", &Pg2SqliteOptions::default()).is_err());
}
