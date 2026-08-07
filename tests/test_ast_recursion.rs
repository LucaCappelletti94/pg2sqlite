//! Tests for recursive translation of AST sub-expressions.
//!
//! Categories covered:
//! 1. Window frame PRECEDING/FOLLOWING bounds
//! 2. FunctionArgumentClause::OrderBy inside aggregates
//! 3. LATERAL VIEW expression translation
//! 4. plpgsql IF-condition translated to SQLite form
//! 5. plpgsql derived-subquery (Derived table factor) translation
//! 6. plpgsql GROUP BY expression inside trigger body

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;
use sqlparser::ast::Statement;

const SCHEMA: &str = "
CREATE TABLE t (
    id INT PRIMARY KEY,
    val FLOAT,
    label TEXT,
    ts TIMESTAMP
);";

/// Translate and return the first `SELECT` statement in the output.
fn translate_ok(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .into_iter()
        .find(|s| matches!(s, Statement::Query(_)))
        .unwrap()
        .to_string()
}

/// Translate and return all statements joined with newlines.
fn translate_all(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// An expression inside a window-frame `PRECEDING` bound must be translated.
/// Here `EXTRACT(EPOCH FROM NOW())` is a PG-specific expression; after
/// translation `NOW()` becomes `datetime('now')` so the output must not
/// contain the raw PG form.
#[test]
fn window_frame_preceding_expr_is_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT SUM(val) OVER (
             ORDER BY id
             ROWS BETWEEN EXTRACT(EPOCH FROM NOW()) PRECEDING AND CURRENT ROW
         )
         FROM t;"
    );
    let out = translate_ok(&sql);
    assert!(!out.contains("now()"), "now() must be translated inside PRECEDING bound: {out}");
    assert!(out.contains("datetime('now')"), "Expected datetime('now') in PRECEDING bound: {out}");
    exec_translated(&sql);
}

/// An expression inside a window-frame `FOLLOWING` bound must be translated.
#[test]
fn window_frame_following_expr_is_translated() {
    let sql = format!(
        "{SCHEMA}
         SELECT SUM(val) OVER (
             ORDER BY id
             ROWS BETWEEN CURRENT ROW AND EXTRACT(EPOCH FROM NOW()) FOLLOWING
         )
         FROM t;"
    );
    let out = translate_ok(&sql);
    assert!(!out.contains("now()"), "now() must be translated inside FOLLOWING bound: {out}");
    assert!(out.contains("datetime('now')"), "Expected datetime('now') in FOLLOWING bound: {out}");
    exec_translated(&sql);
}

/// `date_trunc` inside the `ORDER BY` clause of `string_agg` must be
/// translated to `strftime`.
#[test]
fn string_agg_order_by_clause_translates_date_trunc() {
    let sql = format!(
        "{SCHEMA}
         SELECT string_agg(label, ',' ORDER BY date_trunc('month', ts)) FROM t;"
    );
    let out = translate_ok(&sql);
    assert!(out.contains("group_concat"), "string_agg should rename to group_concat: {out}");
    assert!(
        !out.contains("date_trunc"),
        "date_trunc inside ORDER BY clause must be translated: {out}"
    );
    assert!(out.contains("strftime"), "Expected strftime in the ORDER BY clause: {out}");
    exec_translated(&sql);
}

/// `now()` inside the `ORDER BY` clause of `string_agg` must become
/// `datetime('now')`.
#[test]
fn string_agg_order_by_clause_translates_now() {
    let sql = format!(
        "{SCHEMA}
         SELECT string_agg(label, ',' ORDER BY now()) FROM t;"
    );
    let out = translate_ok(&sql);
    assert!(out.contains("group_concat"), "string_agg should rename to group_concat: {out}");
    assert!(!out.contains("now()"), "now() inside ORDER BY clause must be translated: {out}");
    assert!(
        out.contains("datetime('now')"),
        "Expected datetime('now') in the ORDER BY clause: {out}"
    );
    exec_translated(&sql);
}

/// The expression inside a `LATERAL VIEW` clause must be translated.
/// `now()` should become `datetime('now')` even in a LATERAL VIEW position.
#[test]
fn lateral_view_expr_is_translated() {
    // LATERAL VIEW is a Hive/Spark extension also accepted by sqlparser's
    // PostgreSQL dialect visitor; it populates Select::lateral_views.
    let sql = format!(
        "{SCHEMA}
         SELECT ts_val
         FROM t
         LATERAL VIEW now() v AS ts_val;"
    );
    let out = translate_ok(&sql);
    assert!(!out.contains("now()"), "now() inside LATERAL VIEW must be translated: {out}");
    assert!(out.contains("datetime('now')"), "Expected datetime('now') inside LATERAL VIEW: {out}");
    // Pins R122: LATERAL VIEW passes through and SQLite refuses it. Goes red when
    // the defect is fixed, which is the point.
    {
        let conn = Connection::open_in_memory().unwrap();
        let stmts = Pg2Sqlite::default()
            .sql(&sql)
            .unwrap()
            .translate(&Pg2SqliteOptions::default())
            .unwrap();
        let mut pin_triggered = false;
        for s in &stmts {
            let sql_str = s.to_string();
            let result = conn.execute_batch(&format!("{sql_str};"));
            if let Err(err) = &result {
                assert!(
                    err.to_string().contains("near \"VIEW\": syntax error"),
                    "expected LATERAL VIEW error: {err}"
                );
                pin_triggered = true;
            }
        }
        assert!(pin_triggered, "R122 pin: expected LATERAL VIEW to be refused by SQLite");
    }
}

/// When a plpgsql trigger uses `date_trunc` in an `IF` condition, the
/// condition injected into the WHERE clause must use `strftime`, not
/// `date_trunc`.
#[test]
fn plpgsql_if_condition_date_trunc_is_translated() {
    let sql = "
CREATE TABLE t (id INT PRIMARY KEY, val FLOAT, label TEXT, ts TEXT);

CREATE OR REPLACE FUNCTION fn_if_cond() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF date_trunc('day', NEW.ts::timestamp) = '2023-01-01'::timestamp THEN
        UPDATE t SET label = 'matched' WHERE id = NEW.id;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER tr_if_cond
AFTER INSERT ON t
FOR EACH ROW EXECUTE FUNCTION fn_if_cond();
";
    let out = translate_all(sql);
    assert!(!out.contains("date_trunc"), "date_trunc in IF condition must be translated: {out}");
    assert!(out.contains("strftime"), "Expected strftime in translated IF condition: {out}");
    exec_translated(sql);
}

/// IF / ELSIF: both branches have their conditions translated.
#[test]
fn plpgsql_elsif_condition_is_translated() {
    let sql = "
CREATE TABLE t (id INT PRIMARY KEY, val FLOAT, label TEXT, ts TEXT);

CREATE OR REPLACE FUNCTION fn_elsif_cond() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF date_trunc('month', NEW.ts::timestamp) = '2023-01-01'::timestamp THEN
        UPDATE t SET label = 'jan' WHERE id = NEW.id;
    ELSIF date_trunc('month', NEW.ts::timestamp) = '2023-02-01'::timestamp THEN
        UPDATE t SET label = 'feb' WHERE id = NEW.id;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER tr_elsif_cond
AFTER INSERT ON t
FOR EACH ROW EXECUTE FUNCTION fn_elsif_cond();
";
    let out = translate_all(sql);
    assert!(!out.contains("date_trunc"), "date_trunc in ELSIF condition must be translated: {out}");
    assert!(out.contains("strftime"), "Expected strftime in translated ELSIF condition: {out}");
    exec_translated(sql);
}

/// A subquery in the FROM clause of an INSERT inside a trigger body must have
/// its PG-specific expressions translated.
#[test]
fn plpgsql_derived_subquery_is_translated() {
    let sql = "
CREATE TABLE src (id INT PRIMARY KEY, ts TEXT);
CREATE TABLE dst (id INT PRIMARY KEY, bucket TEXT);

CREATE OR REPLACE FUNCTION fn_derived() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO dst (id, bucket)
    SELECT id, date_trunc('month', ts::timestamp)::text
    FROM (SELECT id, ts FROM src WHERE id = NEW.id) sub;
    RETURN NEW;
END;
$$;

CREATE TRIGGER tr_derived
AFTER INSERT ON src
FOR EACH ROW EXECUTE FUNCTION fn_derived();
";
    let out = translate_all(sql);
    assert!(
        !out.contains("date_trunc"),
        "date_trunc inside derived subquery must be translated: {out}"
    );
    assert!(out.contains("strftime"), "Expected strftime in translated derived subquery: {out}");
    exec_translated(sql);
}

/// A PG-specific expression in a `GROUP BY` inside a trigger's SELECT must
/// be translated to SQLite form.
#[test]
fn plpgsql_group_by_expr_is_translated() {
    let sql = "
CREATE TABLE t (id INT PRIMARY KEY, val FLOAT, label TEXT, ts TEXT);
CREATE TABLE agg (bucket TEXT PRIMARY KEY, total FLOAT);

CREATE OR REPLACE FUNCTION fn_group_by() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO agg (bucket, total)
    SELECT date_trunc('month', ts::timestamp)::text, SUM(val)
    FROM t
    GROUP BY date_trunc('month', ts::timestamp);
    RETURN NEW;
END;
$$;

CREATE TRIGGER tr_group_by
AFTER INSERT ON t
FOR EACH ROW EXECUTE FUNCTION fn_group_by();
";
    let out = translate_all(sql);
    assert!(!out.contains("date_trunc"), "date_trunc in GROUP BY must be translated: {out}");
    assert!(out.contains("strftime"), "Expected strftime in translated GROUP BY: {out}");
    exec_translated(sql);
}

fn exec_translated(pg_sql: &str) {
    let conn = Connection::open_in_memory().unwrap();
    let stmts =
        Pg2Sqlite::default().sql(pg_sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("SQLite rejected: {s}\n{e}"));
    }
}
