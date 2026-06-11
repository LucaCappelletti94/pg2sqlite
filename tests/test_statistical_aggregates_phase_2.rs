//! Phase 2 statistical aggregates: `var_samp` becomes
//! `(sum(x*x) - sum(x)*sum(x)/count(x)) / (count(x) - 1)`, `stddev_samp`
//! wraps that in `sqrt`. PG `variance` and `stddev` alias to those.

mod helpers;

use diesel::{
    Insertable, QueryableByName, connection::SimpleConnection, insert_into, prelude::*, sql_query,
    sql_types::Double, sqlite::SqliteConnection, table,
};
use helpers::{establish_connection, translate_pg};
use pg2sqlite::prelude::Pg2SqliteOptions;

table! {
    /// Univariate test data for Phase 2.
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
    translate_pg(pg, &Pg2SqliteOptions::default()).expect("translation failed").join("\n")
}

/// v in 1..=5. var_samp = 2.5, stddev_samp = sqrt(2.5).
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
fn p2_var_samp_to_closed_form() {
    let out = translate("SELECT var_samp(v) FROM m;");
    assert!(out.contains("sum(v * v)") && out.contains("count(v) - 1"), "{out}");
}

#[test]
fn p2_stddev_samp_wraps_var_samp_in_sqrt() {
    let out = translate("SELECT stddev_samp(v) FROM m;");
    assert!(
        out.contains("sqrt(") && out.contains("sum(v * v)") && out.contains("count(v) - 1"),
        "{out}"
    );
}

#[test]
fn p2_variance_aliases_to_var_samp() {
    let out = translate("SELECT variance(v) FROM m;");
    assert!(out.contains("sum(v * v)") && out.contains("count(v) - 1"), "{out}");
}

#[test]
fn p2_stddev_aliases_to_stddev_samp() {
    let out = translate("SELECT stddev(v) FROM m;");
    assert!(
        out.contains("sqrt(") && out.contains("sum(v * v)") && out.contains("count(v) - 1"),
        "{out}"
    );
}

#[test]
fn p2_var_samp_with_group_by() {
    let out = translate("SELECT g, var_samp(v) FROM m GROUP BY g;");
    assert!(out.contains("sum(v * v)") && out.contains("GROUP BY g"), "{out}");
}

#[test]
fn p2_apply_var_samp_known_dataset() {
    let mut conn = establish_connection();
    seed(&mut conn);
    let r = aggregate(&mut conn, "SELECT var_samp(v) AS r FROM m;");
    assert!((r - 2.5).abs() < 1e-9, "var_samp should be 2.5, got {r}");
}

#[test]
fn p2_apply_stddev_samp_known_dataset() {
    let mut conn = open_with_sqrt();
    seed(&mut conn);
    let r = aggregate(&mut conn, "SELECT stddev_samp(v) AS r FROM m;");
    assert!((r - 2.5_f64.sqrt()).abs() < 1e-9, "stddev_samp should be sqrt(2.5), got {r}");
}

// corr/covar_pop/covar_samp translate via Phase 3. See
// tests/test_statistical_aggregates_phase_3.rs.
