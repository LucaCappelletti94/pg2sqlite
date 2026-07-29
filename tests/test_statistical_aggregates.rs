//! Phase 1 statistical aggregates: `var_pop` becomes `avg(x*x) -
//! avg(x)*avg(x)`, `stddev_pop` wraps that in `sqrt`. Sample-form
//! aggregates and bivariate aggregates live in the Phase 2 and Phase 3
//! test files.

mod helpers;

use diesel::{
    Insertable, QueryableByName, connection::SimpleConnection, insert_into, prelude::*, sql_query,
    sql_types::Double, sqlite::SqliteConnection, table,
};
use helpers::{establish_connection, translate_pg};
use pg2sqlite::prelude::{Pg2SqliteOptions, TranslationOptions};

table! {
    /// Univariate test data for Phase 1.
    m (id) {
        /// Row identifier.
        id -> Integer,
        /// Numeric column the aggregates run over.
        v -> Double,
    }
}

/// Insertable row used to seed the univariate test data.
#[derive(Insertable)]
#[diesel(table_name = m)]
struct NewRow {
    /// Row identifier.
    id: i32,
    /// Numeric value.
    v: f64,
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

/// v in 1..=5. var_pop = 2, stddev_pop = sqrt(2).
fn seed(conn: &mut SqliteConnection) {
    conn.batch_execute(&translate("CREATE TABLE m (id INTEGER PRIMARY KEY, v REAL NOT NULL);"))
        .expect("apply schema");
    let rows: Vec<NewRow> = (1..=5).map(|i| NewRow { id: i, v: f64::from(i) }).collect();
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

#[test]
fn p1_var_pop_to_avg_closed_form() {
    let out = translate("SELECT var_pop(v) FROM m;");
    assert!(out.contains("avg(v * v)") && out.contains("avg(v) * avg(v)"), "{out}");
}

#[test]
fn p1_stddev_pop_wraps_var_pop_in_sqrt() {
    let out = translate("SELECT stddev_pop(v) FROM m;");
    assert!(out.contains("sqrt(") && out.contains("avg(v * v)"), "{out}");
}

#[test]
fn p1_var_pop_with_group_by() {
    let out = translate("SELECT g, var_pop(v) FROM m GROUP BY g;");
    assert!(out.contains("avg(v * v)") && out.contains("GROUP BY g"), "{out}");
}

#[test]
fn p1_apply_var_pop_known_dataset() {
    let mut conn = establish_connection();
    seed(&mut conn);
    let r = aggregate(&mut conn, "SELECT var_pop(v) AS r FROM m;");
    assert!((r - 2.0).abs() < 1e-9, "var_pop should be 2.0, got {r}");
}

#[test]
fn p1_apply_stddev_pop_known_dataset() {
    let mut conn = open_with_sqrt();
    seed(&mut conn);
    let r = aggregate(&mut conn, "SELECT stddev_pop(v) AS r FROM m;");
    assert!((r - 2.0_f64.sqrt()).abs() < 1e-9, "stddev_pop should be sqrt(2), got {r}");
}

// corr/covar_pop/covar_samp translate via Phase 3. See
// tests/test_statistical_aggregates_phase_3.rs.
