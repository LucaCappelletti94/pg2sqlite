//! Tests for reverse translation of expression variants recently added in
//! forward translation but previously missing in reverse expression matches.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, ReverseTranslator};
use sqlparser::{
    ast::{Expr, SelectItem, SetExpr, Statement},
    dialect::PostgreSqlDialect,
    parser::Parser,
};

fn test_schema() -> sql_traits::structs::ParserDB {
    let ddl = "CREATE TABLE t (a INT, b INT, ts TEXT, txt TEXT);";
    Pg2Sqlite::default().sql(ddl).unwrap().build_schema().unwrap()
}

fn parse_first_projection_expr(sql: &str) -> Expr {
    let mut statements = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap();
    let stmt = statements.remove(0);

    let Statement::Query(query) = stmt else {
        panic!("expected query");
    };
    let SetExpr::Select(select) = *query.body else {
        panic!("expected SELECT body");
    };

    match &select.projection[0] {
        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => expr.clone(),
        other => panic!("unexpected projection item: {other:?}"),
    }
}

fn reverse_expr(expr: &Expr) -> String {
    expr.reverse_translate(&test_schema(), &Pg2SqliteOptions::default()).unwrap().to_string()
}

#[test]
fn reverse_is_distinct_from_supported() {
    let expr = parse_first_projection_expr("SELECT a IS DISTINCT FROM b FROM t;");
    let output = reverse_expr(&expr);
    assert!(output.contains("IS DISTINCT FROM"), "Expected IS DISTINCT FROM, got: {output}");
}

#[test]
fn reverse_is_not_distinct_from_supported() {
    let expr = parse_first_projection_expr("SELECT a IS NOT DISTINCT FROM b FROM t;");
    let output = reverse_expr(&expr);
    assert!(
        output.contains("IS NOT DISTINCT FROM"),
        "Expected IS NOT DISTINCT FROM, got: {output}"
    );
}

#[test]
fn reverse_is_unknown_supported() {
    let expr = parse_first_projection_expr("SELECT (a > b) IS UNKNOWN FROM t;");
    let output = reverse_expr(&expr);
    assert!(output.contains("IS UNKNOWN"), "Expected IS UNKNOWN, got: {output}");
}

#[test]
fn reverse_is_not_unknown_supported() {
    let expr = parse_first_projection_expr("SELECT (a > b) IS NOT UNKNOWN FROM t;");
    let output = reverse_expr(&expr);
    assert!(output.contains("IS NOT UNKNOWN"), "Expected IS NOT UNKNOWN, got: {output}");
}

#[test]
fn reverse_at_time_zone_supported() {
    let expr = parse_first_projection_expr("SELECT ts AT TIME ZONE '+02:00' FROM t;");
    let output = reverse_expr(&expr);
    assert!(output.contains("AT TIME ZONE"), "Expected AT TIME ZONE, got: {output}");
}

#[test]
fn reverse_overlay_supported() {
    let expr = parse_first_projection_expr("SELECT OVERLAY(txt PLACING 'X' FROM 2 FOR 1) FROM t;");
    let output = reverse_expr(&expr);
    assert!(output.contains("OVERLAY"), "Expected OVERLAY, got: {output}");
}

#[test]
fn reverse_any_op_supported() {
    let expr = parse_first_projection_expr("SELECT a > ANY(ARRAY[1, 2, 3]) FROM t;");
    let output = reverse_expr(&expr);
    assert!(output.contains("ANY"), "Expected ANY operator, got: {output}");
}

#[test]
fn reverse_all_op_supported() {
    let expr = parse_first_projection_expr("SELECT a <> ALL(ARRAY[1, 2, 3]) FROM t;");
    let output = reverse_expr(&expr);
    assert!(output.contains("ALL"), "Expected ALL operator, got: {output}");
}

#[test]
fn reverse_similar_to_supported() {
    let expr = parse_first_projection_expr("SELECT txt SIMILAR TO '%x%' FROM t;");
    let output = reverse_expr(&expr);
    assert!(output.contains("SIMILAR TO"), "Expected SIMILAR TO, got: {output}");
}

#[test]
fn reverse_is_normalized_supported() {
    let expr = parse_first_projection_expr("SELECT txt IS NORMALIZED FROM t;");
    let output = reverse_expr(&expr);
    assert!(output.contains("IS NORMALIZED"), "Expected IS NORMALIZED, got: {output}");
}
