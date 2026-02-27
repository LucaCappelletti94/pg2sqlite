//! Tests for DISTINCT ON and grouping-set family rewrites.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::ast::Statement;

fn translate(sql: &str) -> Result<Vec<Statement>, Box<dyn std::error::Error>> {
    Ok(Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?)
}

fn query_sql(translated: &[Statement]) -> String {
    translated
        .iter()
        .find(|stmt| matches!(stmt, Statement::Query(_)))
        .expect("expected translated SELECT query")
        .to_string()
}

fn execute_ddl(
    translated: &[Statement],
    conn: &mut SqliteConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    for stmt in translated.iter().filter(|stmt| !matches!(stmt, Statement::Query(_))) {
        diesel::sql_query(stmt.to_string()).execute(conn)?;
    }
    Ok(())
}

#[derive(Debug, QueryableByName)]
struct DistinctOnRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    user_id: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    ts: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    payload: String,
}

#[test]
fn distinct_on_rewrites_to_window_filter() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL,
            ts INTEGER NOT NULL,
            payload TEXT NOT NULL
        );
        SELECT DISTINCT ON (user_id) user_id, ts, payload
        FROM events
        ORDER BY user_id, ts DESC;
    ";

    let translated = translate(sql)?;
    let query = query_sql(&translated);
    let upper = query.to_uppercase();

    assert!(!upper.contains("DISTINCT ON"), "DISTINCT ON should be rewritten: {query}");
    assert!(upper.contains("ROW_NUMBER"), "Expected ROW_NUMBER rewrite: {query}");

    Ok(())
}

#[test]
fn distinct_on_semantic_highest_per_partition() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL,
            ts INTEGER NOT NULL,
            payload TEXT NOT NULL
        );
        SELECT DISTINCT ON (user_id) user_id, ts, payload
        FROM events
        ORDER BY user_id, ts DESC;
    ";

    let translated = translate(sql)?;
    let query = query_sql(&translated);

    let mut conn = SqliteConnection::establish(":memory:")?;
    execute_ddl(&translated, &mut conn)?;

    diesel::sql_query(
        "INSERT INTO events (id, user_id, ts, payload) VALUES
         (1, 1, 10, 'u1-old'),
         (2, 1, 20, 'u1-new'),
         (3, 2, 15, 'u2-only');",
    )
    .execute(&mut conn)?;

    let rows = diesel::sql_query(query).load::<DistinctOnRow>(&mut conn)?;
    assert_eq!(rows.len(), 2, "Expected one row per user_id");

    let first = rows.iter().find(|r| r.user_id == 1).expect("missing user 1");
    assert_eq!(first.ts, 20);
    assert_eq!(first.payload, "u1-new");

    let second = rows.iter().find(|r| r.user_id == 2).expect("missing user 2");
    assert_eq!(second.ts, 15);
    assert_eq!(second.payload, "u2-only");

    Ok(())
}

#[test]
fn distinct_on_wildcard_stays_unsupported() {
    let sql = "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL,
            ts INTEGER NOT NULL
        );
        SELECT DISTINCT ON (user_id) * FROM events ORDER BY user_id, ts DESC;
    ";

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "Wildcard DISTINCT ON should remain unsupported");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("DISTINCT ON") && err.contains("projection"),
        "Expected explicit DISTINCT ON projection error, got: {err}"
    );
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, QueryableByName)]
struct AggregateRow {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    region: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    product: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
    total: Option<i64>,
}

fn sales_fixture_sql() -> &'static str {
    "
    CREATE TABLE sales (
        id INTEGER PRIMARY KEY,
        region TEXT NOT NULL,
        product TEXT NOT NULL,
        amount INTEGER NOT NULL
    );
    "
}

fn load_sales_data(conn: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    diesel::sql_query(
        "INSERT INTO sales (id, region, product, amount) VALUES
         (1, 'North', 'A', 10),
         (2, 'North', 'B', 20),
         (3, 'South', 'A', 30);",
    )
    .execute(conn)?;
    Ok(())
}

#[test]
fn grouping_sets_rewrites_to_union_all() -> Result<(), Box<dyn std::error::Error>> {
    let sql = format!(
        "{}\nSELECT region, product, SUM(amount) AS total
         FROM sales
         GROUP BY GROUPING SETS ((region, product), (region), ());",
        sales_fixture_sql()
    );

    let translated = translate(&sql)?;
    let query = query_sql(&translated);
    let upper = query.to_uppercase();

    assert!(!upper.contains("GROUPING SETS"), "GROUPING SETS should be rewritten: {query}");
    assert!(upper.contains("UNION ALL"), "Expected UNION ALL expansion: {query}");

    Ok(())
}

#[test]
fn grouping_sets_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = format!(
        "{}\nSELECT region, product, SUM(amount) AS total
         FROM sales
         GROUP BY GROUPING SETS ((region, product), (region), ());",
        sales_fixture_sql()
    );
    let translated = translate(&sql)?;
    let query = query_sql(&translated);

    let mut conn = SqliteConnection::establish(":memory:")?;
    execute_ddl(&translated, &mut conn)?;
    load_sales_data(&mut conn)?;

    let mut rows = diesel::sql_query(query).load::<AggregateRow>(&mut conn)?;
    rows.sort();

    let mut expected = vec![
        AggregateRow {
            region: Some("North".to_string()),
            product: Some("A".to_string()),
            total: Some(10),
        },
        AggregateRow {
            region: Some("North".to_string()),
            product: Some("B".to_string()),
            total: Some(20),
        },
        AggregateRow {
            region: Some("South".to_string()),
            product: Some("A".to_string()),
            total: Some(30),
        },
        AggregateRow { region: Some("North".to_string()), product: None, total: Some(30) },
        AggregateRow { region: Some("South".to_string()), product: None, total: Some(30) },
        AggregateRow { region: None, product: None, total: Some(60) },
    ];
    expected.sort();

    assert_eq!(rows, expected);
    Ok(())
}

#[test]
fn rollup_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = format!(
        "{}\nSELECT region, product, SUM(amount) AS total
         FROM sales
         GROUP BY ROLLUP(region, product);",
        sales_fixture_sql()
    );
    let translated = translate(&sql)?;
    let query = query_sql(&translated);

    let mut conn = SqliteConnection::establish(":memory:")?;
    execute_ddl(&translated, &mut conn)?;
    load_sales_data(&mut conn)?;

    let mut rows = diesel::sql_query(query).load::<AggregateRow>(&mut conn)?;
    rows.sort();

    let mut expected = vec![
        AggregateRow {
            region: Some("North".to_string()),
            product: Some("A".to_string()),
            total: Some(10),
        },
        AggregateRow {
            region: Some("North".to_string()),
            product: Some("B".to_string()),
            total: Some(20),
        },
        AggregateRow {
            region: Some("South".to_string()),
            product: Some("A".to_string()),
            total: Some(30),
        },
        AggregateRow { region: Some("North".to_string()), product: None, total: Some(30) },
        AggregateRow { region: Some("South".to_string()), product: None, total: Some(30) },
        AggregateRow { region: None, product: None, total: Some(60) },
    ];
    expected.sort();

    assert_eq!(rows, expected);
    Ok(())
}

#[test]
fn cube_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = format!(
        "{}\nSELECT region, product, SUM(amount) AS total
         FROM sales
         GROUP BY CUBE(region, product);",
        sales_fixture_sql()
    );
    let translated = translate(&sql)?;
    let query = query_sql(&translated);

    let mut conn = SqliteConnection::establish(":memory:")?;
    execute_ddl(&translated, &mut conn)?;
    load_sales_data(&mut conn)?;

    let mut rows = diesel::sql_query(query).load::<AggregateRow>(&mut conn)?;
    rows.sort();

    let mut expected = vec![
        AggregateRow {
            region: Some("North".to_string()),
            product: Some("A".to_string()),
            total: Some(10),
        },
        AggregateRow {
            region: Some("North".to_string()),
            product: Some("B".to_string()),
            total: Some(20),
        },
        AggregateRow {
            region: Some("South".to_string()),
            product: Some("A".to_string()),
            total: Some(30),
        },
        AggregateRow { region: Some("North".to_string()), product: None, total: Some(30) },
        AggregateRow { region: Some("South".to_string()), product: None, total: Some(30) },
        AggregateRow { region: None, product: Some("A".to_string()), total: Some(40) },
        AggregateRow { region: None, product: Some("B".to_string()), total: Some(20) },
        AggregateRow { region: None, product: None, total: Some(60) },
    ];
    expected.sort();

    assert_eq!(rows, expected);
    Ok(())
}

#[test]
fn rollup_with_prefix_group_key_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = format!(
        "{}\nSELECT region, product, SUM(amount) AS total
         FROM sales
         GROUP BY region, ROLLUP(product);",
        sales_fixture_sql()
    );
    let translated = translate(&sql)?;
    let query = query_sql(&translated);

    let mut conn = SqliteConnection::establish(":memory:")?;
    execute_ddl(&translated, &mut conn)?;
    load_sales_data(&mut conn)?;

    let mut rows = diesel::sql_query(query).load::<AggregateRow>(&mut conn)?;
    rows.sort();

    // With prefix key `region`, rollup on product yields:
    // (region, product) and (region), but no grand total row.
    let mut expected = vec![
        AggregateRow {
            region: Some("North".to_string()),
            product: Some("A".to_string()),
            total: Some(10),
        },
        AggregateRow {
            region: Some("North".to_string()),
            product: Some("B".to_string()),
            total: Some(20),
        },
        AggregateRow {
            region: Some("South".to_string()),
            product: Some("A".to_string()),
            total: Some(30),
        },
        AggregateRow { region: Some("North".to_string()), product: None, total: Some(30) },
        AggregateRow { region: Some("South".to_string()), product: None, total: Some(30) },
    ];
    expected.sort();

    assert_eq!(rows, expected);
    Ok(())
}

#[test]
fn grouping_rewrite_rejects_non_aggregate_non_group_projection() {
    let sql = format!(
        "{}\nSELECT region, product, amount + 1 AS weird
         FROM sales
         GROUP BY ROLLUP(region, product);",
        sales_fixture_sql()
    );

    let result = Pg2Sqlite::default().sql(&sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "Expected strict error for unsupported projection shape");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("GROUPING SETS/ROLLUP/CUBE") || err.contains("aggregate"),
        "Expected strict rewrite-shape error, got: {err}"
    );
}
