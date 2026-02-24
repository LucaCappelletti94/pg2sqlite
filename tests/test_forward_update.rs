//! Tests for forward UPDATE translation in
//! `src/impls/translator_impls/update.rs`.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::{
    ast::{
        BinaryOperator, Expr, Ident, Join, JoinConstraint, JoinOperator, ObjectName,
        ObjectNamePart, Statement, TableFactor,
    },
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

#[test]
fn forward_update_basic() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        UPDATE users SET name = 'Bob' WHERE id = 1;
    ";
    let output = translate(sql);
    assert!(output.contains("UPDATE"), "Expected UPDATE in output: {output}");
    assert!(output.contains("name = 'Bob'"), "Expected updated value in output: {output}");
}

#[test]
fn forward_update_from_is_preserved() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, team_id INT, name TEXT);
        CREATE TABLE teams (id INT PRIMARY KEY, name TEXT);
        UPDATE users SET name = teams.name
        FROM teams
        WHERE users.team_id = teams.id;
    ";
    let output = translate(sql);
    assert!(output.contains("UPDATE"), "Expected UPDATE in output: {output}");
    assert!(output.contains("FROM teams"), "Expected FROM clause in output: {output}");
}

#[test]
fn forward_update_translates_assignment_expressions() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, updated_at TEXT);
        UPDATE users SET updated_at = now() WHERE id = 1;
    ";
    let output = translate(sql);
    assert!(
        output.contains("datetime('now')"),
        "Expected now() to translate to datetime('now'): {output}"
    );
}

#[test]
fn update_with_joined_target_table_is_rejected() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, team_id INT, name TEXT);
        CREATE TABLE teams (id INT PRIMARY KEY, name TEXT);
    ";

    let mut update_stmt =
        Parser::parse_sql(&PostgreSqlDialect {}, "UPDATE users SET name = 'x' WHERE id = 1;")
            .unwrap()
            .pop()
            .unwrap();

    let Statement::Update(update) = &mut update_stmt else {
        panic!("Expected UPDATE statement");
    };

    update.table.joins.push(Join {
        relation: TableFactor::Table {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("teams"))]),
            alias: None,
            args: None,
            with_hints: vec![],
            version: None,
            partitions: vec![],
            json_path: None,
            sample: None,
            index_hints: vec![],
            with_ordinality: false,
        },
        global: false,
        join_operator: JoinOperator::Inner(JoinConstraint::On(Expr::BinaryOp {
            left: Box::new(Expr::CompoundIdentifier(vec![
                Ident::new("users"),
                Ident::new("team_id"),
            ])),
            op: BinaryOperator::Eq,
            right: Box::new(Expr::CompoundIdentifier(vec![Ident::new("teams"), Ident::new("id")])),
        })),
    });

    let result = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .statement(update_stmt)
        .translate(&Pg2SqliteOptions::default());

    assert!(result.is_err(), "Expected unsupported UPDATE target join error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("UPDATE with joins on the target table"),
        "Expected target-table-join error, got: {err}"
    );
}

#[test]
fn forward_update_limit_translates_expressions() {
    let schema_sql = "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);";
    let mut update_stmt =
        Parser::parse_sql(&PostgreSqlDialect {}, "UPDATE users SET name = 'x' WHERE id = 1;")
            .unwrap()
            .remove(0);

    let Statement::Update(update) = &mut update_stmt else {
        panic!("Expected UPDATE statement");
    };
    update.limit = Some(parse_expr("NOW()"));

    let output = Pg2Sqlite::default()
        .sql(schema_sql)
        .unwrap()
        .statement(update_stmt)
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        output.contains("LIMIT") && output.contains("datetime('now')"),
        "Expected translated LIMIT expression in UPDATE: {output}"
    );
}
