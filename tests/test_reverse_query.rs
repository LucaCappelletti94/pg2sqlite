//! Tests for reverse query translation in
//! `src/impls/reverse_translator_impls/query.rs`.
//!
//! Covers: ORDER BY, SetOperation (UNION/INTERSECT), Values, HAVING, GROUP BY,
//! Derived table factor, NestedJoin, ExprWithAlias, JoinConstraint::Using,
//! and various join types.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn reverse(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();
    assert!(!stmts.is_empty());
    stmts[0].to_string()
}

const SCHEMA: &str = "CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);";
const TWO_TABLES: &str = "
    CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
    CREATE TABLE posts (id INT PRIMARY KEY, user_id INT, title TEXT);
";

#[test]
fn reverse_order_by_asc() {
    let pg = reverse(SCHEMA, "SELECT * FROM users ORDER BY name ASC;");
    assert!(pg.contains("ORDER BY"), "Expected ORDER BY: {pg}");
}

#[test]
fn reverse_order_by_desc() {
    let pg = reverse(SCHEMA, "SELECT * FROM users ORDER BY age DESC;");
    assert!(pg.contains("ORDER BY"), "Expected ORDER BY: {pg}");
    assert!(pg.contains("DESC"), "Expected DESC: {pg}");
}

#[test]
fn reverse_order_by_multiple() {
    let pg = reverse(SCHEMA, "SELECT * FROM users ORDER BY name ASC, age DESC;");
    assert!(pg.contains("ORDER BY"), "Expected ORDER BY: {pg}");
}

#[test]
fn reverse_union() {
    let pg = reverse(SCHEMA, "SELECT id FROM users UNION SELECT id FROM users;");
    assert!(pg.contains("UNION"), "Expected UNION: {pg}");
}

#[test]
fn reverse_union_all() {
    let pg = reverse(SCHEMA, "SELECT id FROM users UNION ALL SELECT id FROM users;");
    assert!(pg.contains("UNION ALL"), "Expected UNION ALL: {pg}");
}

#[test]
fn reverse_intersect() {
    let pg = reverse(SCHEMA, "SELECT id FROM users INTERSECT SELECT id FROM users;");
    assert!(pg.contains("INTERSECT"), "Expected INTERSECT: {pg}");
}

#[test]
fn reverse_except() {
    let pg = reverse(SCHEMA, "SELECT id FROM users EXCEPT SELECT id FROM users;");
    assert!(pg.contains("EXCEPT"), "Expected EXCEPT: {pg}");
}

#[test]
fn reverse_having() {
    let pg = reverse(SCHEMA, "SELECT age, COUNT(*) FROM users GROUP BY age HAVING COUNT(*) > 1;");
    assert!(pg.contains("HAVING"), "Expected HAVING: {pg}");
}

#[test]
fn reverse_group_by() {
    let pg = reverse(SCHEMA, "SELECT age, COUNT(*) FROM users GROUP BY age;");
    assert!(pg.contains("GROUP BY"), "Expected GROUP BY: {pg}");
}

#[test]
fn reverse_derived_table() {
    let pg = reverse(SCHEMA, "SELECT * FROM (SELECT id, name FROM users) AS sub;");
    assert!(pg.contains("SELECT"), "Expected subquery: {pg}");
    assert!(pg.contains("sub"), "Expected alias 'sub': {pg}");
}

#[test]
fn reverse_expr_with_alias() {
    let pg = reverse(SCHEMA, "SELECT name AS user_name FROM users;");
    assert!(pg.contains("user_name"), "Expected alias 'user_name': {pg}");
}

#[test]
fn reverse_inner_join() {
    let pg = reverse(
        TWO_TABLES,
        "SELECT u.name, p.title FROM users u INNER JOIN posts p ON u.id = p.user_id;",
    );
    assert!(pg.contains("JOIN"), "Expected JOIN: {pg}");
}

#[test]
fn reverse_left_join() {
    let pg = reverse(
        TWO_TABLES,
        "SELECT u.name, p.title FROM users u LEFT JOIN posts p ON u.id = p.user_id;",
    );
    assert!(pg.contains("LEFT"), "Expected LEFT JOIN: {pg}");
}

#[test]
fn reverse_cross_join() {
    let pg = reverse(TWO_TABLES, "SELECT * FROM users CROSS JOIN posts;");
    assert!(pg.contains("CROSS JOIN"), "Expected CROSS JOIN: {pg}");
}

#[test]
fn reverse_right_join() {
    let pg = reverse(
        TWO_TABLES,
        "SELECT u.name, p.title FROM users u RIGHT JOIN posts p ON u.id = p.user_id;",
    );
    assert!(pg.contains("RIGHT"), "Expected RIGHT JOIN: {pg}");
}

#[test]
fn reverse_right_outer_join() {
    let pg = reverse(
        TWO_TABLES,
        "SELECT u.name, p.title FROM users u RIGHT OUTER JOIN posts p ON u.id = p.user_id;",
    );
    assert!(pg.contains("RIGHT OUTER") || pg.contains("RIGHT"), "Expected RIGHT OUTER JOIN: {pg}");
}

#[test]
fn reverse_full_outer_join() {
    let pg = reverse(
        TWO_TABLES,
        "SELECT u.name, p.title FROM users u FULL OUTER JOIN posts p ON u.id = p.user_id;",
    );
    assert!(
        pg.contains("FULL OUTER") || pg.contains("FULL JOIN"),
        "Expected FULL OUTER JOIN: {pg}"
    );
}

#[test]
fn reverse_natural_join() {
    let pg = reverse(TWO_TABLES, "SELECT * FROM users NATURAL JOIN posts;");
    assert!(pg.contains("NATURAL"), "Expected NATURAL JOIN: {pg}");
}

#[test]
fn reverse_join_using() {
    let pg = reverse(
        "CREATE TABLE t1 (id INT PRIMARY KEY, name TEXT);
         CREATE TABLE t2 (id INT PRIMARY KEY, value TEXT);",
        "SELECT * FROM t1 JOIN t2 USING (id);",
    );
    assert!(pg.contains("USING"), "Expected USING: {pg}");
}

#[test]
fn reverse_nested_join() {
    let pg = reverse(
        "CREATE TABLE t1 (id INT PRIMARY KEY, name TEXT);
         CREATE TABLE t2 (id INT PRIMARY KEY, t1_id INT);
         CREATE TABLE t3 (id INT PRIMARY KEY, t2_id INT);",
        "SELECT * FROM (t1 JOIN t2 ON t1.id = t2.t1_id) JOIN t3 ON t2.id = t3.t2_id;",
    );
    assert!(pg.contains("JOIN"), "Expected nested JOIN: {pg}");
}

#[test]
fn reverse_limit() {
    let pg = reverse(SCHEMA, "SELECT * FROM users LIMIT 10;");
    assert!(pg.contains("LIMIT"), "Expected LIMIT: {pg}");
}

#[test]
fn reverse_limit_offset() {
    let pg = reverse(SCHEMA, "SELECT * FROM users LIMIT 10 OFFSET 5;");
    assert!(pg.contains("LIMIT"), "Expected LIMIT: {pg}");
    assert!(pg.contains("OFFSET"), "Expected OFFSET: {pg}");
}

#[test]
fn reverse_distinct() {
    let pg = reverse(SCHEMA, "SELECT DISTINCT name FROM users;");
    assert!(pg.contains("DISTINCT"), "Expected DISTINCT: {pg}");
}

#[test]
fn reverse_insert_values() {
    let pg = reverse(
        SCHEMA,
        "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30), (2, 'Bob', 25);",
    );
    assert!(pg.contains("VALUES"), "Expected VALUES: {pg}");
    assert!(pg.contains("Alice"), "Expected Alice: {pg}");
    assert!(pg.contains("Bob"), "Expected Bob: {pg}");
}

#[test]
fn reverse_left_outer_join_explicit() {
    let pg = reverse(
        TWO_TABLES,
        "SELECT * FROM users LEFT OUTER JOIN posts ON users.id = posts.user_id;",
    );
    assert!(pg.contains("LEFT"), "Expected LEFT join: {pg}");
    assert!(pg.contains("JOIN"), "Expected JOIN: {pg}");
}

#[test]
fn reverse_cross_join_explicit() {
    let pg = reverse(TWO_TABLES, "SELECT * FROM users CROSS JOIN posts;");
    assert!(pg.contains("CROSS JOIN"), "Expected CROSS JOIN: {pg}");
}

#[test]
fn reverse_subquery_in_select() {
    let pg = reverse(
        TWO_TABLES,
        "SELECT name, (SELECT COUNT(*) FROM posts WHERE posts.user_id = users.id) AS post_count FROM users;",
    );
    assert!(pg.contains("SELECT"), "Expected SELECT: {pg}");
    assert!(pg.contains("COUNT"), "Expected COUNT: {pg}");
}

#[test]
fn reverse_multiple_aliases() {
    let pg = reverse(SCHEMA, "SELECT name AS user_name, age AS user_age FROM users;");
    assert!(pg.contains("user_name"), "Expected alias user_name: {pg}");
    assert!(pg.contains("user_age"), "Expected alias user_age: {pg}");
}

#[test]
fn reverse_group_by_multiple() {
    let pg = reverse(
        TWO_TABLES,
        "SELECT users.name, posts.title, COUNT(*) FROM users JOIN posts ON users.id = posts.user_id GROUP BY users.name, posts.title;",
    );
    assert!(pg.contains("GROUP BY"), "Expected GROUP BY: {pg}");
}

#[test]
fn reverse_having_with_sum() {
    let pg =
        reverse(SCHEMA, "SELECT name, SUM(age) FROM users GROUP BY name HAVING SUM(age) > 100;");
    assert!(pg.contains("HAVING"), "Expected HAVING: {pg}");
    assert!(pg.contains("SUM"), "Expected SUM: {pg}");
}

#[test]
fn reverse_subquery_from_with_filter() {
    let pg = reverse(
        SCHEMA,
        "SELECT * FROM (SELECT id, name FROM users WHERE age > 18) AS adults WHERE adults.name LIKE 'A%';",
    );
    assert!(pg.contains("adults"), "Expected alias: {pg}");
    assert!(pg.contains("LIKE"), "Expected LIKE: {pg}");
}

#[test]
fn reverse_union_with_order_by() {
    let pg = reverse(
        SCHEMA,
        "SELECT id, name FROM users WHERE age > 30 UNION ALL SELECT id, name FROM users WHERE age < 10 ORDER BY name;",
    );
    assert!(pg.contains("UNION ALL"), "Expected UNION ALL: {pg}");
    assert!(pg.contains("ORDER BY"), "Expected ORDER BY: {pg}");
}

#[test]
fn reverse_nested_join_complex() {
    let pg = reverse(
        "CREATE TABLE a (id INT PRIMARY KEY, val TEXT);
         CREATE TABLE b (id INT PRIMARY KEY, a_id INT);
         CREATE TABLE c (id INT PRIMARY KEY, b_id INT);",
        "SELECT * FROM a JOIN (b JOIN c ON b.id = c.b_id) ON a.id = b.a_id;",
    );
    assert!(pg.contains("JOIN"), "Expected JOIN: {pg}");
}

#[test]
fn reverse_select_wildcard() {
    let pg = reverse(SCHEMA, "SELECT * FROM users;");
    assert!(pg.contains('*'), "Expected wildcard: {pg}");
}

#[test]
fn reverse_insert_from_join() {
    let pg = reverse(
        TWO_TABLES,
        "INSERT INTO users (id, name, age) SELECT p.id, p.title, 0 FROM posts p JOIN users u ON p.user_id = u.id;",
    );
    assert!(pg.contains("INSERT"), "Expected INSERT: {pg}");
    assert!(pg.contains("JOIN"), "Expected JOIN: {pg}");
}

#[test]
fn reverse_named_window_spec_and_reference() {
    let pg = reverse(
        TWO_TABLES,
        "SELECT SUM(u.id) OVER w2
         FROM users u
         WINDOW w1 AS (PARTITION BY u.id ORDER BY u.id),
                w2 AS (w1);",
    );
    assert!(pg.contains("WINDOW"), "Expected WINDOW clause: {pg}");
    assert!(pg.contains("w1") && pg.contains("w2"), "Expected named windows: {pg}");
}
