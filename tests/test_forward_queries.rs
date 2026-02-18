//! Tests for forward query translation covering join types and other query
//! paths in `src/impls/translator_impls/query.rs`.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

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

// ==================== Views with joins ====================

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

// ==================== Views with subqueries ====================

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

// ==================== Views with set operations ====================

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

// ==================== View with GROUP BY / HAVING ====================

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

// ==================== View with ORDER BY / LIMIT ====================

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

// ==================== Multi-join view ====================

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

// ==================== Nested join ====================

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

// ==================== Join with USING ====================

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

// ==================== INSERT...SELECT with JOIN (exercises query.rs join
// operators) ====================

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

// ==================== View body expression translation ====================

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

// ==================== GROUP BY / HAVING expression translation
// ====================

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

// ==================== CTE expression translation ====================

#[test]
fn cte_body_translates_expressions() {
    let sql = "
        CREATE TABLE events (id INT PRIMARY KEY, name TEXT);
        WITH cte AS (SELECT NOW() AS ts FROM events) SELECT * FROM cte;
    ";
    let output = translate(sql);
    assert!(output.contains("datetime('now')"), "Expected datetime('now') in CTE body: {output}");
}
