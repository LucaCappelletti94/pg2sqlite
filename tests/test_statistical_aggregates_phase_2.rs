//! Red tests for sample-form statistical aggregates.
//!
//! Phase 2: `var_samp` becomes
//! `(sum(x*x) - sum(x)*sum(x)/count(x)) / (count(x) - 1)`, `stddev_samp`
//! wraps that in `sqrt`. PG `variance` aliases to `var_samp` and PG
//! `stddev` aliases to `stddev_samp`, so both produce the same form.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .and_then(|t| t.translate(&Pg2SqliteOptions::default()))
        .map_or_else(
            |e| panic!("translation failed: {e}"),
            |stmts| stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"),
        )
}

fn try_translate(sql: &str) -> Result<String, pg2sqlite::errors::Error> {
    Pg2Sqlite::default()
        .sql(sql)
        .and_then(|t| t.translate(&Pg2SqliteOptions::default()))
        .map(|stmts| stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
}

// Phase 2: var_samp / stddev_samp closed forms + variance/stddev aliases

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
    use rusqlite::Connection;
    // var_samp(1..5) = sum of squared deviations / (n - 1) = 10 / 4 = 2.5
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&translate("CREATE TABLE m (id INTEGER PRIMARY KEY, v REAL);")).unwrap();
    conn.execute_batch("INSERT INTO m (id, v) VALUES (1,1),(2,2),(3,3),(4,4),(5,5);").unwrap();
    let q = translate("SELECT var_samp(v) FROM m;");
    let r: f64 = conn.query_row(&q, [], |row| row.get(0)).unwrap();
    assert!((r - 2.5).abs() < 1e-9, "var_samp should be 2.5, got {r}");
}

#[test]
fn p2_apply_stddev_samp_known_dataset() {
    use rusqlite::{Connection, functions::FunctionFlags};
    let conn = Connection::open_in_memory().unwrap();
    conn.create_scalar_function(
        "sqrt",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(ctx.get::<f64>(0)?.sqrt()),
    )
    .unwrap();
    conn.execute_batch(&translate("CREATE TABLE m (id INTEGER PRIMARY KEY, v REAL);")).unwrap();
    conn.execute_batch("INSERT INTO m (id, v) VALUES (1,1),(2,2),(3,3),(4,4),(5,5);").unwrap();
    let q = translate("SELECT stddev_samp(v) FROM m;");
    let r: f64 = conn.query_row(&q, [], |row| row.get(0)).unwrap();
    assert!((r - 2.5_f64.sqrt()).abs() < 1e-9, "stddev_samp should be sqrt(2.5), got {r}");
}

// Guards: later phases stay unsupported

#[test]
fn corr_stays_unsupported() {
    assert!(try_translate("SELECT corr(v, w) FROM m;").is_err());
}

#[test]
fn covar_pop_stays_unsupported() {
    assert!(try_translate("SELECT covar_pop(v, w) FROM m;").is_err());
}

#[test]
fn covar_samp_stays_unsupported() {
    assert!(try_translate("SELECT covar_samp(v, w) FROM m;").is_err());
}
