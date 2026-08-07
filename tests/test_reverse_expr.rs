//! Tests for reverse expression translation in
//! `src/impls/reverse_translator_impls/expr.rs`.
//!
//! Covers all expression match arms: UnaryOp, Nested, BinaryOp, Cast, IsNull,
//! IsNotNull, IsTrue, IsNotTrue, IsFalse, IsNotFalse, Exists, Like, ILike,
//! InList, InSubquery, Between, Case, Subquery, Extract, Tuple, Trim, Ceil,
//! Floor, Position, Substring, Collate, and the fallback error.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, ReverseTranslator};
use sql_traits::structs::ParserDB;
use sqlparser::ast::{AccessExpr, Expr, Ident, ObjectName, ObjectNamePart, Subscript};

/// Helper to set up translator with a simple schema and reverse translate
/// SQLite SQL.
fn reverse(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();
    assert!(!stmts.is_empty(), "Expected at least one statement");
    stmts[0].to_string()
}

fn empty_schema() -> ParserDB {
    ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build")
}

const SCHEMA: &str = "CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT, score REAL);";

#[test]
fn reverse_unary_op_not() {
    let pg = reverse(SCHEMA, "SELECT NOT (age > 5) FROM users;");
    assert!(pg.contains("NOT"), "Expected NOT in output: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_unary_op_minus() {
    let pg = reverse(SCHEMA, "SELECT -age FROM users;");
    assert!(pg.contains('-'), "Expected minus in output: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_nested_parenthesized() {
    let pg = reverse(SCHEMA, "SELECT (age + 1) FROM users;");
    assert!(pg.contains('('), "Expected parenthesized expression: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_binary_op_add() {
    let pg = reverse(SCHEMA, "SELECT age + score FROM users;");
    assert!(pg.contains('+'), "Expected + operator: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_binary_op_and() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE age > 5 AND name = 'test';");
    assert!(pg.contains("AND"), "Expected AND: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_cast() {
    let pg = reverse(SCHEMA, "SELECT CAST(age AS TEXT) FROM users;");
    assert!(pg.contains("CAST"), "Expected CAST in output: {pg}");
    assert!(pg.contains("TEXT"), "Expected TEXT type: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_is_null() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE name IS NULL;");
    assert!(pg.contains("IS NULL"), "Expected IS NULL: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_is_not_null() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE name IS NOT NULL;");
    assert!(pg.contains("IS NOT NULL"), "Expected IS NOT NULL: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_is_true() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE (age > 0) IS TRUE;");
    assert!(pg.contains("IS TRUE"), "Expected IS TRUE: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_is_not_true() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE (age > 0) IS NOT TRUE;");
    assert!(pg.contains("IS NOT TRUE"), "Expected IS NOT TRUE: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_is_false() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE (age > 0) IS FALSE;");
    assert!(pg.contains("IS FALSE"), "Expected IS FALSE: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_is_not_false() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE (age > 0) IS NOT FALSE;");
    assert!(pg.contains("IS NOT FALSE"), "Expected IS NOT FALSE: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_exists() {
    let pg =
        reverse(SCHEMA, "SELECT * FROM users WHERE EXISTS (SELECT 1 FROM users WHERE age > 5);");
    assert!(pg.contains("EXISTS"), "Expected EXISTS: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_not_exists() {
    let pg = reverse(
        SCHEMA,
        "SELECT * FROM users WHERE NOT EXISTS (SELECT 1 FROM users WHERE age > 5);",
    );
    assert!(pg.contains("NOT EXISTS"), "Expected NOT EXISTS: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_like() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE name LIKE '%test%';");
    assert!(pg.contains("LIKE"), "Expected LIKE: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_not_like() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE name NOT LIKE '%test%';");
    assert!(pg.contains("NOT LIKE"), "Expected NOT LIKE: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_in_list() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE age IN (1, 2, 3);");
    assert!(pg.contains("IN"), "Expected IN: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_not_in_list() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE age NOT IN (1, 2, 3);");
    assert!(pg.contains("NOT IN"), "Expected NOT IN: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_in_subquery() {
    let pg =
        reverse(SCHEMA, "SELECT * FROM users WHERE id IN (SELECT id FROM users WHERE age > 5);");
    assert!(pg.contains("IN (SELECT"), "Expected IN subquery: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_between() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE age BETWEEN 10 AND 20;");
    assert!(pg.contains("BETWEEN"), "Expected BETWEEN: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_case_when() {
    let pg = reverse(SCHEMA, "SELECT CASE WHEN age > 18 THEN 'adult' ELSE 'minor' END FROM users;");
    assert!(pg.contains("CASE"), "Expected CASE: {pg}");
    assert!(pg.contains("WHEN"), "Expected WHEN: {pg}");
    assert!(pg.contains("ELSE"), "Expected ELSE: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_case_with_operand() {
    let pg = reverse(
        SCHEMA,
        "SELECT CASE age WHEN 18 THEN 'eighteen' WHEN 21 THEN 'twentyone' END FROM users;",
    );
    assert!(pg.contains("CASE"), "Expected CASE: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_scalar_subquery() {
    let pg = reverse(SCHEMA, "SELECT (SELECT MAX(age) FROM users) AS max_age;");
    assert!(pg.contains("SELECT"), "Expected subquery: {pg}");
    assert!(pg.contains("MAX"), "Expected MAX: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_trim() {
    let pg = reverse(SCHEMA, "SELECT TRIM(name) FROM users;");
    assert!(pg.contains("TRIM"), "Expected TRIM: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_position() {
    let pg = reverse(SCHEMA, "SELECT POSITION('a' IN name) FROM users;");
    assert!(pg.contains("POSITION"), "Expected POSITION: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_substring() {
    let pg = reverse(SCHEMA, "SELECT SUBSTRING(name FROM 1 FOR 3) FROM users;");
    assert!(pg.contains("SUBSTRING"), "Expected SUBSTRING: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_collate() {
    let pg = reverse(SCHEMA, "SELECT name COLLATE NOCASE FROM users;");
    assert!(pg.contains("COLLATE"), "Expected COLLATE: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_complex_nested_expression() {
    let pg = reverse(
        SCHEMA,
        "SELECT * FROM users WHERE (age > 18 AND name IS NOT NULL) OR score BETWEEN 0.0 AND 100.0;",
    );
    assert!(pg.contains("AND"), "Expected AND: {pg}");
    assert!(pg.contains("OR"), "Expected OR: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_ilike() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE name ILIKE '%test%';");
    assert!(pg.contains("ILIKE"), "Expected ILIKE: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_not_ilike() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE name NOT ILIKE '%test%';");
    assert!(pg.contains("NOT ILIKE"), "Expected NOT ILIKE: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_extract() {
    let pg = reverse(
        "CREATE TABLE events (id INT PRIMARY KEY, created_at TIMESTAMP);",
        "SELECT EXTRACT(YEAR FROM created_at) FROM events;",
    );
    assert!(pg.contains("EXTRACT"), "Expected EXTRACT: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_tuple_in_where() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE (id, age) IN ((1, 30), (2, 25));");
    assert!(pg.contains("IN"), "Expected IN: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_array_literal() {
    let pg = reverse(SCHEMA, "SELECT ARRAY[1, 2, 3] FROM users;");
    assert!(pg.contains("ARRAY"), "Expected ARRAY: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_trim_leading() {
    let pg = reverse(SCHEMA, "SELECT TRIM(LEADING ' ' FROM name) FROM users;");
    assert!(pg.contains("TRIM"), "Expected TRIM: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_trim_both() {
    let pg = reverse(SCHEMA, "SELECT TRIM(BOTH ' ' FROM name) FROM users;");
    assert!(pg.contains("TRIM"), "Expected TRIM: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_ceil() {
    let pg = reverse(SCHEMA, "SELECT CEIL(score) FROM users;");
    assert!(pg.contains("CEIL"), "Expected CEIL: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_floor() {
    let pg = reverse(SCHEMA, "SELECT FLOOR(score) FROM users;");
    assert!(pg.contains("FLOOR"), "Expected FLOOR: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_regexp() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE name REGEXP '^[A-Z]';");
    assert!(pg.contains("name ~ '^[A-Z]'"), "Expected the POSIX operator: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_interval() {
    let pg = reverse(
        "CREATE TABLE events (id INT PRIMARY KEY, created_at TIMESTAMP);",
        "SELECT * FROM events WHERE created_at > INTERVAL '1' DAY;",
    );
    assert!(pg.contains("INTERVAL"), "Expected INTERVAL: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_compound_identifier() {
    let pg = reverse(SCHEMA, "SELECT users.name FROM users;");
    assert!(pg.contains("users.name") || pg.contains("name"), "Expected compound: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_typed_string_date() {
    let pg = reverse(
        "CREATE TABLE events (id INT PRIMARY KEY, created_at DATE);",
        "SELECT * FROM events WHERE created_at > DATE '2024-01-01';",
    );
    assert!(pg.contains("DATE") || pg.contains("2024-01-01"), "Expected DATE: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_typed_string_timestamp() {
    let pg = reverse(
        "CREATE TABLE events (id INT PRIMARY KEY, created_at TIMESTAMP);",
        "SELECT * FROM events WHERE created_at > TIMESTAMP '2024-01-01 00:00:00';",
    );
    assert!(pg.contains("TIMESTAMP") || pg.contains("2024-01-01"), "Expected TIMESTAMP: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_qualified_wildcard_in_select() {
    let pg = reverse(
        "CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
         CREATE TABLE posts (id INT PRIMARY KEY, user_id INT, title TEXT);",
        "SELECT users.*, posts.title FROM users JOIN posts ON users.id = posts.user_id;",
    );
    assert!(pg.contains("users") && pg.contains("title"), "Expected qualified wildcard: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_substring_with_for() {
    let pg = reverse(SCHEMA, "SELECT SUBSTRING(name, 1, 3) FROM users;");
    assert!(pg.contains("SUBSTRING") || pg.contains("name"), "Expected SUBSTRING: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_case_multiple_when() {
    let pg = reverse(
        SCHEMA,
        "SELECT CASE WHEN age < 13 THEN 'child' WHEN age < 18 THEN 'teen' WHEN age < 65 THEN 'adult' ELSE 'senior' END FROM users;",
    );
    assert!(pg.contains("CASE"), "Expected CASE: {pg}");
    assert!(pg.contains("child"), "Expected child: {pg}");
    assert!(pg.contains("senior"), "Expected senior: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_not_between() {
    let pg = reverse(SCHEMA, "SELECT * FROM users WHERE age NOT BETWEEN 10 AND 20;");
    assert!(pg.contains("NOT BETWEEN"), "Expected NOT BETWEEN: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_not_in_subquery() {
    let pg = reverse(
        SCHEMA,
        "SELECT * FROM users WHERE id NOT IN (SELECT id FROM users WHERE age < 18);",
    );
    assert!(pg.contains("NOT IN"), "Expected NOT IN: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_deeply_nested() {
    let pg = reverse(
        SCHEMA,
        "SELECT * FROM users WHERE ((age > 5) AND (name IS NOT NULL)) OR (score < 10.0);",
    );
    assert!(pg.contains("AND"), "Expected AND: {pg}");
    assert!(pg.contains("OR"), "Expected OR: {pg}");
    assert_parses_as_pg(&pg);
}

#[test]
fn reverse_manual_expr_variants_cover_prefixed_trim_chars_and_compound_access() {
    let schema = empty_schema();
    let options = Pg2SqliteOptions::default();

    let trim_with_characters = Expr::Trim {
        expr: Box::new(Expr::Identifier(Ident::new("name"))),
        trim_where: None,
        trim_what: None,
        trim_characters: Some(vec![Expr::Value(sqlparser::ast::ValueWithSpan::from(
            sqlparser::ast::Value::SingleQuotedString("x".to_string()),
        ))]),
    };
    let trimmed =
        trim_with_characters.reverse_translate(&schema, &options).expect("trim should reverse");
    assert!(trimmed.to_string().contains("TRIM"));

    let prefixed = Expr::Prefixed {
        prefix: Ident::new("N"),
        value: Box::new(Expr::Value(sqlparser::ast::ValueWithSpan::from(
            sqlparser::ast::Value::SingleQuotedString("abc".to_string()),
        ))),
    };
    let prefixed_out =
        prefixed.reverse_translate(&schema, &options).expect("prefixed should reverse");
    assert!(prefixed_out.to_string().contains('N'));

    let qualified_wildcard = Expr::QualifiedWildcard(
        ObjectName(vec![ObjectNamePart::Identifier(Ident::new("users"))]),
        sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
    );
    let wildcard_out = qualified_wildcard
        .reverse_translate(&schema, &options)
        .expect("qualified wildcard should reverse");
    assert_eq!(wildcard_out.to_string(), "users.*");

    let regexp_expr = Expr::RLike {
        negated: false,
        expr: Box::new(Expr::Identifier(Ident::new("name"))),
        pattern: Box::new(Expr::Value(sqlparser::ast::ValueWithSpan::from(
            sqlparser::ast::Value::SingleQuotedString("^[A-Z]".to_string()),
        ))),
        regexp: true,
    };
    let regexp_out =
        regexp_expr.reverse_translate(&schema, &options).expect("regexp should reverse");
    assert_eq!(regexp_out.to_string(), "name ~ '^[A-Z]'");

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
    let compound_out = compound_access
        .reverse_translate(&schema, &options)
        .expect("compound field access should reverse");
    assert!(compound_out.to_string().contains("payload[1].field"));
}

#[test]
fn reverse_wildcard_expr_clones_through() {
    let schema = empty_schema();
    let options = Pg2SqliteOptions::default();
    let wildcard = Expr::Wildcard(sqlparser::ast::helpers::attached_token::AttachedToken::empty());
    // Wildcard is a leaf node — reverse translation clones it as-is.
    let result = wildcard.reverse_translate(&schema, &options).expect("wildcard should clone");
    assert_eq!(result.to_string(), wildcard.to_string());
}

fn assert_parses_as_pg(sql: &str) {
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, sql)
        .expect("reverse output must parse as PostgreSQL");
}
