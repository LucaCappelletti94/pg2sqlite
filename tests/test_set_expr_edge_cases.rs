//! Tests for SetExpr edge cases: TABLE and MERGE → error in forward,
//! passthrough in reverse.

use pg2sqlite::prelude::{Pg2SqliteOptions, ReverseTranslator, Translator};
use sql_traits::structs::ParserDB;
use sqlparser::ast::{Query, SetExpr, Statement};

fn make_table_query() -> Query {
    let mut query = base_query();
    *query.body = SetExpr::Table(Box::new(sqlparser::ast::Table {
        table_name: Some("foo".to_string()),
        schema_name: None,
    }));
    query
}

fn base_query() -> Query {
    use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};
    let stmt = Parser::parse_sql(&PostgreSqlDialect {}, "SELECT 1").expect("parse").remove(0);
    if let Statement::Query(q) = stmt {
        *q
    } else {
        panic!("expected query");
    }
}

fn make_merge_query() -> Query {
    use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};
    let merge_stmt = Parser::parse_sql(
        &PostgreSqlDialect {},
        "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET c = 1",
    )
    .expect("parse")
    .remove(0);
    let mut query = base_query();
    *query.body = SetExpr::Merge(merge_stmt);
    query
}

fn schema() -> ParserDB {
    ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build")
}

#[test]
fn table_expr_forward_returns_error() {
    let schema = schema();
    let options = Pg2SqliteOptions::default();
    let query = make_table_query();

    let result = query.translate(&schema, &options);
    assert!(result.is_err(), "TABLE expression should error in forward translation");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("TABLE") || err.contains("MERGE"),
        "Error should mention TABLE/MERGE: {err}"
    );
}

#[test]
fn merge_expr_forward_returns_error() {
    let schema = schema();
    let options = Pg2SqliteOptions::default();
    let query = make_merge_query();

    let result = query.translate(&schema, &options);
    assert!(result.is_err(), "MERGE expression should error in forward translation");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("TABLE") || err.contains("MERGE"),
        "Error should mention TABLE/MERGE: {err}"
    );
}

#[test]
fn table_expr_reverse_passthrough() {
    let schema = schema();
    let options = Pg2SqliteOptions::default();
    let query = make_table_query();

    let result =
        query.reverse_translate(&schema, &pg2sqlite::options::TranslationContext::new(&options));
    assert!(result.is_ok(), "Reverse should pass through TABLE: {:?}", result.err());
}

#[test]
fn merge_expr_reverse_passthrough() {
    let schema = schema();
    let options = Pg2SqliteOptions::default();
    let query = make_merge_query();

    let result =
        query.reverse_translate(&schema, &pg2sqlite::options::TranslationContext::new(&options));
    assert!(result.is_ok(), "Reverse should pass through MERGE: {:?}", result.err());
}
