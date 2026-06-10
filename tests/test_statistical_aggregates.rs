//! Red tests for PG statistical aggregates lowered to SQLite closed forms.
//!
//! Phase 1: `var_pop` becomes `avg(x*x) - avg(x)*avg(x)`, `stddev_pop`
//! wraps that in `sqrt`. Sample-form (`var_samp`, `stddev_samp`,
//! `variance`, `stddev`), `covar_*`, and `corr` come in later phases.

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

// Phase 1: var_pop and stddev_pop closed forms

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
    use rusqlite::Connection;
    // var_pop(1..5) = mean((x - mean)^2) = (4+1+0+1+4)/5 = 2
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&translate("CREATE TABLE m (id INTEGER PRIMARY KEY, v REAL);")).unwrap();
    conn.execute_batch("INSERT INTO m (id, v) VALUES (1,1),(2,2),(3,3),(4,4),(5,5);").unwrap();
    let q = translate("SELECT var_pop(v) FROM m;");
    let r: f64 = conn.query_row(&q, [], |row| row.get(0)).unwrap();
    assert!((r - 2.0).abs() < 1e-9, "var_pop should be 2.0, got {r}");
}

#[test]
fn p1_apply_stddev_pop_known_dataset() {
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&translate("CREATE TABLE m (id INTEGER PRIMARY KEY, v REAL);")).unwrap();
    conn.execute_batch("INSERT INTO m (id, v) VALUES (1,1),(2,2),(3,3),(4,4),(5,5);").unwrap();
    let q = translate("SELECT stddev_pop(v) FROM m;");
    let r: f64 = conn.query_row(&q, [], |row| row.get(0)).unwrap();
    assert!((r - 2.0_f64.sqrt()).abs() < 1e-9, "stddev_pop should be sqrt(2), got {r}");
}

// Guards: later phases stay unsupported until they ship

#[test]
fn stddev_samp_stays_unsupported_in_phase_1() {
    assert!(try_translate("SELECT stddev_samp(v) FROM m;").is_err());
}

#[test]
fn variance_alias_stays_unsupported_in_phase_1() {
    // PG `variance` aliases to var_samp (sample form), part of Phase 2.
    assert!(try_translate("SELECT variance(v) FROM m;").is_err());
}

#[test]
fn corr_stays_unsupported_in_phase_1() {
    assert!(try_translate("SELECT corr(v, w) FROM m;").is_err());
}
