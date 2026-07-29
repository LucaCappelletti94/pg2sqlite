//! Tests for forward query translation covering join types and other query
//! paths in `src/impls/translator_impls/query.rs`.

mod helpers;

use diesel::{RunQueryDsl, SqliteConnection, prelude::*};
use helpers::translate_statements;
use pg2sqlite::{
    errors::Error,
    options::Pg2SqliteOptions,
    prelude::{Pg2Sqlite, Translator},
};
use sql_traits::structs::ParserDB;
use sqlparser::ast::{Expr, Ident, OrderByExpr, OrderByOptions, Value, ValueWithSpan, WithFill};

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

const SCHEMA: &str = "
    CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
    CREATE TABLE posts (id INT PRIMARY KEY, user_id INT, title TEXT);
    CREATE TABLE comments (id INT PRIMARY KEY, post_id INT, body TEXT);
";

#[test]
fn view_with_inner_join() {
    let sql = format!(
        "{SCHEMA}
         CREATE VIEW user_posts AS
         SELECT u.name, p.title
         FROM users u INNER JOIN posts p ON u.id = p.user_id;"
    );
    let output = translate(&sql);
    assert!(output.contains("JOIN"), "Expected JOIN: {output}");
}

#[test]
fn view_with_left_join() {
    let sql = format!(
        "{SCHEMA}
         CREATE VIEW user_posts AS
         SELECT u.name, p.title
         FROM users u LEFT JOIN posts p ON u.id = p.user_id;"
    );
    let output = translate(&sql);
    assert!(output.contains("LEFT"), "Expected LEFT JOIN: {output}");
}

#[test]
fn view_with_right_join() {
    let sql = format!(
        "{SCHEMA}
         CREATE VIEW user_posts AS
         SELECT u.name, p.title
         FROM users u RIGHT JOIN posts p ON u.id = p.user_id;"
    );
    let output = translate(&sql);
    assert!(output.contains("RIGHT"), "Expected RIGHT JOIN: {output}");
}

#[test]
fn view_with_full_outer_join() {
    let sql = format!(
        "{SCHEMA}
         CREATE VIEW user_posts AS
         SELECT u.name, p.title
         FROM users u FULL OUTER JOIN posts p ON u.id = p.user_id;"
    );
    let output = translate(&sql);
    assert!(
        output.contains("FULL") || output.contains("OUTER"),
        "Expected FULL OUTER JOIN: {output}"
    );
}

#[test]
fn view_with_cross_join() {
    let sql = format!(
        "{SCHEMA}
         CREATE VIEW user_posts AS
         SELECT u.name, p.title
         FROM users u CROSS JOIN posts p;"
    );
    let output = translate(&sql);
    assert!(output.contains("CROSS JOIN"), "Expected CROSS JOIN: {output}");
}

#[test]
fn view_with_natural_join() {
    let sql = format!(
        "{SCHEMA}
         CREATE VIEW combo AS
         SELECT * FROM users NATURAL JOIN posts;"
    );
    let output = translate(&sql);
    assert!(output.contains("NATURAL"), "Expected NATURAL JOIN: {output}");
}

#[test]
fn view_with_derived_table() {
    let sql = format!(
        "{SCHEMA}
         CREATE VIEW top_users AS
         SELECT * FROM (SELECT id, name FROM users WHERE age > 18) AS adults;"
    );
    let output = translate(&sql);
    assert!(output.contains("adults"), "Expected derived table alias: {output}");
}

#[test]
fn view_with_union() {
    let sql = format!(
        "{SCHEMA}
         CREATE VIEW all_content AS
         SELECT id, title AS content FROM posts
         UNION
         SELECT id, body AS content FROM comments;"
    );
    let output = translate(&sql);
    assert!(output.contains("UNION"), "Expected UNION: {output}");
}

#[test]
fn view_with_group_by_having() {
    let sql = format!(
        "{SCHEMA}
         CREATE VIEW user_post_counts AS
         SELECT u.name, COUNT(p.id) as post_count
         FROM users u LEFT JOIN posts p ON u.id = p.user_id
         GROUP BY u.name
         HAVING COUNT(p.id) > 0;"
    );
    let output = translate(&sql);
    assert!(output.contains("GROUP BY"), "Expected GROUP BY: {output}");
    assert!(output.contains("HAVING"), "Expected HAVING: {output}");
}

#[test]
fn view_with_order_by() {
    let sql = format!(
        "{SCHEMA}
         CREATE VIEW sorted_users AS
         SELECT * FROM users ORDER BY name ASC, age DESC;"
    );
    let output = translate(&sql);
    assert!(output.contains("ORDER BY"), "Expected ORDER BY: {output}");
}

#[test]
fn view_with_multiple_joins() {
    let sql = format!(
        "{SCHEMA}
         CREATE VIEW full_data AS
         SELECT u.name, p.title, c.body
         FROM users u
         JOIN posts p ON u.id = p.user_id
         JOIN comments c ON p.id = c.post_id;"
    );
    let output = translate(&sql);
    assert!(output.contains("JOIN"), "Expected JOINs: {output}");
    assert!(output.contains("full_data"), "Expected view name: {output}");
}

#[test]
fn view_with_nested_join() {
    let sql = format!(
        "{SCHEMA}
         CREATE VIEW nested AS
         SELECT *
         FROM (users JOIN posts ON users.id = posts.user_id)
         JOIN comments ON posts.id = comments.post_id;"
    );
    let output = translate(&sql);
    assert!(output.contains("JOIN"), "Expected nested JOIN: {output}");
}

#[test]
fn view_with_using_join() {
    let sql = "
        CREATE TABLE t1 (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE t2 (id INT PRIMARY KEY, value TEXT);
        CREATE VIEW joined AS SELECT * FROM t1 JOIN t2 USING (id);
    ";
    let output = translate(sql);
    assert!(output.contains("USING"), "Expected USING: {output}");
}

#[test]
fn insert_select_with_inner_join() {
    let sql = format!(
        "{SCHEMA}
         CREATE TABLE results (id INT PRIMARY KEY, name TEXT, title TEXT);
         INSERT INTO results SELECT u.id, u.name, p.title FROM users u INNER JOIN posts p ON u.id = p.user_id;"
    );
    let output = translate(&sql);
    assert!(output.contains("INSERT INTO"), "Expected INSERT INTO: {output}");
    assert!(output.contains("JOIN"), "Expected JOIN: {output}");
}

#[test]
fn insert_select_with_left_join() {
    let sql = format!(
        "{SCHEMA}
         CREATE TABLE results (id INT PRIMARY KEY, name TEXT, title TEXT);
         INSERT INTO results SELECT u.id, u.name, p.title FROM users u LEFT JOIN posts p ON u.id = p.user_id;"
    );
    let output = translate(&sql);
    assert!(output.contains("LEFT"), "Expected LEFT: {output}");
}

#[test]
fn insert_select_with_left_outer_join() {
    let sql = format!(
        "{SCHEMA}
         CREATE TABLE results (id INT PRIMARY KEY, name TEXT, title TEXT);
         INSERT INTO results SELECT u.id, u.name, p.title FROM users u LEFT OUTER JOIN posts p ON u.id = p.user_id;"
    );
    let output = translate(&sql);
    assert!(output.contains("LEFT"), "Expected LEFT: {output}");
}

#[test]
fn insert_select_with_right_join() {
    let sql = format!(
        "{SCHEMA}
         CREATE TABLE results (id INT PRIMARY KEY, name TEXT, title TEXT);
         INSERT INTO results SELECT u.id, u.name, p.title FROM users u RIGHT JOIN posts p ON u.id = p.user_id;"
    );
    let output = translate(&sql);
    assert!(output.contains("RIGHT"), "Expected RIGHT: {output}");
}

#[test]
fn insert_select_with_full_outer_join() {
    let sql = format!(
        "{SCHEMA}
         CREATE TABLE results (id INT PRIMARY KEY, name TEXT, title TEXT);
         INSERT INTO results SELECT u.id, u.name, p.title FROM users u FULL OUTER JOIN posts p ON u.id = p.user_id;"
    );
    let output = translate(&sql);
    assert!(output.contains("FULL"), "Expected FULL: {output}");
}

#[test]
fn insert_select_with_cross_join() {
    let sql = format!(
        "{SCHEMA}
         CREATE TABLE results (id INT PRIMARY KEY, name TEXT, title TEXT);
         INSERT INTO results SELECT u.id, u.name, p.title FROM users u CROSS JOIN posts p;"
    );
    let output = translate(&sql);
    assert!(output.contains("CROSS JOIN"), "Expected CROSS JOIN: {output}");
}

#[test]
fn insert_select_with_natural_join() {
    let sql = "
        CREATE TABLE t1 (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE t2 (id INT PRIMARY KEY, value TEXT);
        CREATE TABLE results (id INT PRIMARY KEY, name TEXT, value TEXT);
        INSERT INTO results SELECT * FROM t1 NATURAL JOIN t2;
    ";
    let output = translate(sql);
    assert!(output.contains("NATURAL"), "Expected NATURAL: {output}");
}

#[test]
fn insert_select_with_derived_table() {
    let sql = format!(
        "{SCHEMA}
         CREATE TABLE results (id INT PRIMARY KEY, name TEXT, age INT);
         INSERT INTO results SELECT * FROM (SELECT * FROM users WHERE age > 18) AS adults;"
    );
    let output = translate(&sql);
    assert!(output.contains("INSERT INTO"), "Expected INSERT INTO: {output}");
}

#[test]
fn insert_select_with_using_join() {
    let sql = "
        CREATE TABLE t1 (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE t2 (id INT PRIMARY KEY, value TEXT);
        CREATE TABLE results (id INT PRIMARY KEY, name TEXT, value TEXT);
        INSERT INTO results SELECT * FROM t1 JOIN t2 USING (id);
    ";
    let output = translate(sql);
    assert!(output.contains("USING"), "Expected USING join: {output}");
}

#[test]
fn view_body_translates_now() {
    let sql = "
        CREATE TABLE events (id INT PRIMARY KEY, name TEXT);
        CREATE VIEW recent_events AS
        SELECT *, NOW() AS created FROM events;
    ";
    let output = translate(sql);
    assert!(output.contains("datetime('now')"), "Expected datetime('now') in view body: {output}");
}

#[test]
fn group_by_translates_expressions() {
    let sql = "
        CREATE TABLE events (id INT PRIMARY KEY, name TEXT, ts TEXT);
        SELECT NOW(), COUNT(*) FROM events GROUP BY NOW();
    ";
    let output = translate(sql);
    assert!(output.contains("GROUP BY datetime('now')"), "Expected translated GROUP BY: {output}");
}

#[test]
fn having_translates_expressions() {
    let sql = "
        CREATE TABLE events (id INT PRIMARY KEY, name TEXT, ts TEXT);
        SELECT ts, COUNT(*) FROM events GROUP BY ts HAVING NOW() > ts;
    ";
    let output = translate(sql);
    assert!(output.contains("HAVING datetime('now')"), "Expected translated HAVING: {output}");
}

#[test]
fn cte_body_translates_expressions() {
    let sql = "
        CREATE TABLE events (id INT PRIMARY KEY, name TEXT);
        WITH cte AS (SELECT NOW() AS ts FROM events) SELECT * FROM cte;
    ";
    let output = translate(sql);
    assert!(output.contains("datetime('now')"), "Expected datetime('now') in CTE body: {output}");
}

#[test]
fn distinct_on_returns_error() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        SELECT DISTINCT ON (id) * FROM users;
    ";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "Expected error for DISTINCT ON");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("DISTINCT ON"), "Expected DISTINCT ON error: {err}");
}

#[test]
fn named_window_translates_expressions() {
    let sql = "
        CREATE TABLE events (id INT PRIMARY KEY, ts TEXT);
        SELECT *, ROW_NUMBER() OVER w FROM events WINDOW w AS (PARTITION BY NOW() ORDER BY id);
    ";
    let output = translate(sql);
    assert!(
        output.contains("datetime('now')"),
        "Expected datetime('now') in named window: {output}"
    );
}

fn empty_schema() -> ParserDB {
    ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build")
}

#[test]
fn order_by_expr_translation_rejects_with_fill_clause() {
    let schema = empty_schema();
    let options = Pg2SqliteOptions::default();
    let order_by_expr = OrderByExpr {
        expr: Expr::Identifier(Ident::new("created_at")),
        options: OrderByOptions::default(),
        with_fill: Some(WithFill {
            from: Some(Expr::Value(ValueWithSpan::from(Value::Number("1".to_string(), false)))),
            to: None,
            step: None,
        }),
    };

    let error =
        order_by_expr.translate(&schema, &options).expect_err("WITH FILL should not be supported");
    assert!(matches!(
        error,
        Error::UnknownPostgresFeature(message) if message == "WITH FILL in ORDER BY"
    ));
}

#[test]
fn set_expr_insert_translates_expressions() {
    let options = Pg2SqliteOptions::default();
    let result = translate_statements(
        "SELECT * FROM (INSERT INTO t (name) VALUES ('test') RETURNING *) AS sub",
        &options,
    );
    // The key point is it doesn't panic
    let _ = result;
}

diesel::table! {
    /// Products table for testing qualified wildcards.
    products (id) {
        /// Product ID.
        id -> Integer,
        /// Product name.
        name -> Text,
        /// Product price.
        price -> Nullable<Double>,
    }
}

/// A product record.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = products)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Product {
    /// Product ID.
    id: i32,
    /// Product name.
    name: String,
    /// Product price.
    price: Option<f64>,
}

#[test]
fn test_wildcard_select_column_translation() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE products (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            price REAL
        );
        SELECT products.* FROM products;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    assert!(
        select_stmt.contains("products.*"),
        "Should contain qualified wildcard, got: {select_stmt}"
    );

    Ok(())
}

#[test]
fn test_wildcard_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE products (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            price REAL
        );
        SELECT products.* FROM products;
    ";

    let options = Pg2SqliteOptions::default();
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection = SqliteConnection::establish(":memory:").expect("Failed to connect");

    // Execute DDL
    for stmt in translated.iter().filter(|s| !matches!(s, sqlparser::ast::Statement::Query(_))) {
        diesel::sql_query(&stmt.to_string()).execute(&mut connection)?;
    }

    // Insert test data
    diesel::insert_into(products::table)
        .values(&Product { id: 1, name: "Widget".to_string(), price: Some(9.99) })
        .execute(&mut connection)?;
    diesel::insert_into(products::table)
        .values(&Product { id: 2, name: "Gadget".to_string(), price: Some(19.99) })
        .execute(&mut connection)?;

    let select_stmt = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Should have a SELECT statement")
        .to_string();

    #[derive(diesel::QueryableByName, Debug)]
    struct ProductResult {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        name: String,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Double>)]
        price: Option<f64>,
    }

    let results = diesel::sql_query(&select_stmt).load::<ProductResult>(&mut connection)?;

    assert_eq!(results.len(), 2, "Should have 2 products");
    assert_eq!(results[0].id, 1);
    assert_eq!(results[0].name, "Widget");
    assert!((results[0].price.unwrap() - 9.99).abs() < 0.01);
    assert_eq!(results[1].id, 2);
    assert_eq!(results[1].name, "Gadget");
    assert!((results[1].price.unwrap() - 19.99).abs() < 0.01);

    Ok(())
}
