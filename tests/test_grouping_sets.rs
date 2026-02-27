//! Tests for GROUPING SETS, ROLLUP, and CUBE expansion to UNION ALL.

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
