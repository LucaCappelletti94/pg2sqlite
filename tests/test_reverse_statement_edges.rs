//! Additional reverse-statement edge coverage.

use pg2sqlite::{
    prelude::{Pg2SqliteOptions, ReverseTranslator},
    traits::TranslationOptions,
};
use sql_traits::structs::ParserDB;
use sqlparser::{
    ast::{
        AccessExpr, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgOperator,
        FunctionArgumentList, FunctionArguments, GroupByExpr, Ident, LimitClause, ObjectName,
        ObjectNamePart, Offset, OffsetRows, Query, SelectItem, SetExpr, Statement, Subscript,
        TableObject,
    },
    dialect::PostgreSqlDialect,
    parser::Parser,
};

fn parse_statements(sql: &str) -> Vec<Statement> {
    Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse")
}

fn parse_statement(sql: &str) -> Statement {
    parse_statements(sql).remove(0)
}

fn parse_query(sql: &str) -> Query {
    let stmt = parse_statement(sql);
    if let Statement::Query(query) = stmt {
        *query
    } else {
        panic!("expected query statement");
    }
}

fn parse_insert(sql: &str) -> sqlparser::ast::Insert {
    let stmt = parse_statement(sql);
    if let Statement::Insert(insert) = stmt {
        insert
    } else {
        panic!("expected insert statement");
    }
}

fn parse_expr(sql: &str) -> Expr {
    Parser::new(&PostgreSqlDialect {}).try_with_sql(sql).unwrap().parse_expr().unwrap()
}

fn schema() -> ParserDB {
    ParserDB::from_statements(
        parse_statements(
            r#"
            CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT, payload TEXT);
            CREATE TABLE posts(id INTEGER PRIMARY KEY, user_id INTEGER, title TEXT);
            "#,
        ),
        "test".to_string(),
    )
    .expect("schema should build")
}

fn table_function(name: &str) -> Function {
    Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))]),
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
        uses_odbc_syntax: false,
    }
}

#[test]
fn reverse_statement_covers_table_function_and_query_setexpr_variants() {
    let schema = schema();
    let options = Pg2SqliteOptions::default();

    // PostgreSQL inserts into a table or a view, never into a call, so this
    // shape is refused rather than carried across as SQL the server cannot
    // parse.
    let mut insert_with_table_fn = parse_insert("INSERT INTO users(id, name) VALUES (1, 'a')");
    insert_with_table_fn.table = TableObject::TableFunction(table_function("remote_users"));
    let error = Statement::Insert(insert_with_table_fn)
        .reverse_translate(&schema, &options)
        .expect_err("a table-function INSERT has no PostgreSQL form");
    assert!(
        error.to_string().contains("table function is not supported"),
        "the refusal names the shape, got: {error}"
    );

    let mut base_query = parse_query("SELECT 1");
    for set_expr in [
        SetExpr::Insert(parse_statement("INSERT INTO users(id, name) VALUES (1, 'a')")),
        SetExpr::Update(parse_statement("UPDATE users SET name = 'x'")),
        SetExpr::Delete(parse_statement("DELETE FROM users WHERE id = 1")),
    ] {
        *base_query.body = set_expr;
        let stmt = Statement::Query(Box::new(base_query.clone()));
        let out = stmt.reverse_translate(&schema, &options).expect("query should reverse");
        assert!(matches!(out, Statement::Query(_)));
    }

    let delete_with_using =
        parse_statement("DELETE FROM users USING posts WHERE users.id = posts.user_id");
    let out = delete_with_using
        .reverse_translate(&schema, &options)
        .expect("delete using should reverse");
    assert!(matches!(out, Statement::Delete(_)));

    let mut delete_with_tables = parse_statement("DELETE FROM users WHERE id = 1");
    if let Statement::Delete(delete) = &mut delete_with_tables {
        delete.tables = vec![ObjectName(vec![ObjectNamePart::Identifier(Ident::new("users"))])];
    }
    let out = delete_with_tables
        .reverse_translate(&schema, &options)
        .expect("delete tables list should reverse");
    assert!(matches!(out, Statement::Delete(_)));
}

#[test]
fn reverse_statement_covers_expression_and_limit_checker_variants() {
    let schema = schema();
    // The subject is the expression walker, so the placeholder name is
    // declared. An undeclared one is refused, which
    // `tests/test_reverse_unknown_functions.rs` pins.
    let options = Pg2SqliteOptions::default().with_user_defined_functions(["custom_fn"]);

    let trim_expr = Expr::Trim {
        expr: Box::new(Expr::Identifier(Ident::new("name"))),
        trim_where: None,
        trim_what: None,
        trim_characters: Some(vec![Expr::Value(sqlparser::ast::ValueWithSpan::from(
            sqlparser::ast::Value::SingleQuotedString("x".to_string()),
        ))]),
    };
    let substring_expr = Expr::Substring {
        expr: Box::new(Expr::Identifier(Ident::new("name"))),
        substring_from: Some(Box::new(Expr::Value(sqlparser::ast::ValueWithSpan::from(
            sqlparser::ast::Value::Number("1".to_string(), false),
        )))),
        substring_for: Some(Box::new(Expr::Value(sqlparser::ast::ValueWithSpan::from(
            sqlparser::ast::Value::Number("2".to_string(), false),
        )))),
        special: true,
        shorthand: true,
    };
    let overlay_expr = Expr::Overlay {
        expr: Box::new(Expr::Identifier(Ident::new("name"))),
        overlay_what: Box::new(Expr::Value(sqlparser::ast::ValueWithSpan::from(
            sqlparser::ast::Value::SingleQuotedString("x".to_string()),
        ))),
        overlay_from: Box::new(Expr::Value(sqlparser::ast::ValueWithSpan::from(
            sqlparser::ast::Value::Number("1".to_string(), false),
        ))),
        overlay_for: Some(Box::new(Expr::Value(sqlparser::ast::ValueWithSpan::from(
            sqlparser::ast::Value::Number("1".to_string(), false),
        )))),
    };
    let compound_access = Expr::CompoundFieldAccess {
        root: Box::new(Expr::Identifier(Ident::new("payload"))),
        access_chain: vec![
            AccessExpr::Subscript(Subscript::Index {
                index: Expr::Value(sqlparser::ast::ValueWithSpan::from(
                    sqlparser::ast::Value::Number("1".to_string(), false),
                )),
            }),
            AccessExpr::Dot(Expr::Identifier(Ident::new("field"))),
        ],
    };

    let function_expr = Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("custom_fn"))]),
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                FunctionArg::Named {
                    name: Ident::new("named"),
                    arg: FunctionArgExpr::Expr(Expr::Identifier(Ident::new("name"))),
                    operator: FunctionArgOperator::RightArrow,
                },
                FunctionArg::ExprNamed {
                    name: Expr::Identifier(Ident::new("expr_name")),
                    arg: FunctionArgExpr::Expr(Expr::Identifier(Ident::new("payload"))),
                    operator: FunctionArgOperator::Equals,
                },
            ],
            clauses: vec![],
        }),
        filter: Some(Box::new(parse_expr("1 = 1"))),
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
        uses_odbc_syntax: false,
    });

    for expr in [trim_expr, substring_expr, overlay_expr, compound_access, function_expr] {
        let mut query = parse_query("SELECT 1 FROM users");
        if let SetExpr::Select(select) = query.body.as_mut() {
            select.projection = vec![SelectItem::UnnamedExpr(expr)];
            select.group_by = GroupByExpr::Expressions(vec![parse_expr("1")], vec![]);
        }
        query.limit_clause = Some(LimitClause::LimitOffset {
            limit: Some(parse_expr("10")),
            offset: Some(Offset { value: parse_expr("1"), rows: OffsetRows::None }),
            limit_by: vec![parse_expr("2")],
        });
        let stmt = Statement::Query(Box::new(query));
        let out = stmt
            .reverse_translate(&schema, &options)
            .expect("query with expression should reverse");
        assert!(matches!(out, Statement::Query(_)));
    }
}
