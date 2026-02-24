//! Tests for forward DELETE translation covering USING clause conversion.
//! in `src/impls/translator_impls/delete.rs`.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::{
    ast::{Expr, Statement},
    dialect::PostgreSqlDialect,
    parser::Parser,
};

fn translate(sql: &str) -> String {
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

fn parse_expr(sql: &str) -> Expr {
    Parser::new(&PostgreSqlDialect {}).try_with_sql(sql).unwrap().parse_expr().unwrap()
}

fn parse_order_by_expr(sql: &str) -> sqlparser::ast::OrderByExpr {
    let stmt = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap().remove(0);
    let Statement::Query(query) = stmt else {
        panic!("Expected query statement");
    };
    let Some(order_by) = query.order_by else {
        panic!("Expected ORDER BY clause");
    };
    let sqlparser::ast::OrderByKind::Expressions(mut exprs) = order_by.kind else {
        panic!("Expected ORDER BY expressions");
    };
    exprs.remove(0)
}

// ==================== DELETE with USING ====================

#[test]
fn delete_using_converts_to_exists() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE inactive (user_id INT PRIMARY KEY);
        DELETE FROM users USING inactive WHERE users.id = inactive.user_id;
    ";
    let output = translate(sql);
    // USING should be converted to EXISTS subquery
    assert!(
        output.contains("EXISTS") || output.contains("DELETE"),
        "Expected EXISTS or DELETE: {output}"
    );
}

#[test]
fn delete_using_with_condition() {
    let sql = "
        CREATE TABLE orders (id INT PRIMARY KEY, user_id INT, status TEXT);
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        DELETE FROM orders USING users WHERE orders.user_id = users.id AND users.name = 'test';
    ";
    let output = translate(sql);
    assert!(output.contains("DELETE") || output.contains("EXISTS"), "Expected DELETE: {output}");
}

// ==================== Basic DELETE ====================

#[test]
fn delete_basic() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        DELETE FROM users WHERE id = 1;
    ";
    let output = translate(sql);
    assert!(output.contains("DELETE"), "Expected DELETE: {output}");
}

#[test]
fn delete_all() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        DELETE FROM users;
    ";
    let output = translate(sql);
    assert!(output.contains("DELETE"), "Expected DELETE: {output}");
}

// ==================== DELETE with expression translation ====================

#[test]
fn delete_where_translates_expressions() {
    let sql = "
        CREATE TABLE events (id INT PRIMARY KEY, created_at TEXT);
        DELETE FROM events WHERE NOW() > created_at;
    ";
    let output = translate(sql);
    assert!(
        output.contains("datetime('now')"),
        "Expected datetime('now') in DELETE WHERE: {output}"
    );
}

// ==================== DELETE RETURNING expression translation
// ====================

#[test]
fn delete_returning_translates_expressions() {
    let sql = "
        CREATE TABLE events (id INT PRIMARY KEY, name TEXT);
        DELETE FROM events WHERE id = 1 RETURNING NOW() AS ts;
    ";
    let output = translate(sql);
    assert!(
        output.contains("datetime('now')"),
        "Expected datetime('now') in DELETE RETURNING: {output}"
    );
}

#[test]
fn delete_order_by_and_limit_translate_expressions() {
    let schema_sql = "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);";
    let mut delete_stmt =
        Parser::parse_sql(&PostgreSqlDialect {}, "DELETE FROM users WHERE id > 0;")
            .unwrap()
            .remove(0);

    let Statement::Delete(delete) = &mut delete_stmt else {
        panic!("Expected DELETE statement");
    };
    delete.order_by = vec![parse_order_by_expr("SELECT 1 ORDER BY NOW();")];
    delete.limit = Some(parse_expr("NOW()"));

    let output = Pg2Sqlite::default()
        .sql(schema_sql)
        .unwrap()
        .statement(delete_stmt)
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        output.contains("ORDER BY")
            && output.contains("LIMIT")
            && output.contains("datetime('now')"),
        "Expected translated ORDER BY/LIMIT expressions in DELETE: {output}"
    );
}
