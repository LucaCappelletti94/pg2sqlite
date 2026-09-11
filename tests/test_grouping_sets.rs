//! Tests for GROUPING SETS, ROLLUP, and CUBE expansion to UNION ALL.

mod helpers;

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

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
fn grouping_sets_rewrites_to_union_all() {
    let sql = format!(
        "{}\nSELECT region, product, SUM(amount) AS total
         FROM sales
         GROUP BY GROUPING SETS ((region, product), (region), ());",
        sales_fixture_sql()
    );

    let options = Pg2SqliteOptions::default();
    let query = helpers::prepared_user_select(&sql, &options);
    let upper = query.to_uppercase();

    assert!(!upper.contains("GROUPING SETS"), "GROUPING SETS should be rewritten: {query}");
    assert!(upper.contains("UNION ALL"), "Expected UNION ALL expansion: {query}");
}

#[test]
fn grouping_sets_semantic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = format!(
        "{}\nSELECT region, product, SUM(amount) AS total
         FROM sales
         GROUP BY GROUPING SETS ((region, product), (region), ());",
        sales_fixture_sql()
    );
    let options = Pg2SqliteOptions::default();
    let stmts = helpers::translate_pg(&sql, &options).unwrap();
    let query = helpers::user_statement_of(&stmts, "SELECT").clone();
    let mut conn = SqliteConnection::establish(":memory:")?;
    // Dynamically-generated DDL; the table schema is ephemeral and has no
    // table! macro.
    for s in stmts.iter().filter(|s| !helpers::is_user_statement(s, "SELECT")) {
        diesel::sql_query(s.as_str()).execute(&mut conn)?;
    }
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
    let options = Pg2SqliteOptions::default();
    let stmts = helpers::translate_pg(&sql, &options).unwrap();
    let query = helpers::user_statement_of(&stmts, "SELECT").clone();
    let mut conn = SqliteConnection::establish(":memory:")?;
    // Dynamically-generated DDL; the table schema is ephemeral and has no
    // table! macro.
    for s in stmts.iter().filter(|s| !helpers::is_user_statement(s, "SELECT")) {
        diesel::sql_query(s.as_str()).execute(&mut conn)?;
    }
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
    let options = Pg2SqliteOptions::default();
    let stmts = helpers::translate_pg(&sql, &options).unwrap();
    let query = helpers::user_statement_of(&stmts, "SELECT").clone();
    let mut conn = SqliteConnection::establish(":memory:")?;
    // Dynamically-generated DDL; the table schema is ephemeral and has no
    // table! macro.
    for s in stmts.iter().filter(|s| !helpers::is_user_statement(s, "SELECT")) {
        diesel::sql_query(s.as_str()).execute(&mut conn)?;
    }
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
    let options = Pg2SqliteOptions::default();
    let stmts = helpers::translate_pg(&sql, &options).unwrap();
    let query = helpers::user_statement_of(&stmts, "SELECT").clone();
    let mut conn = SqliteConnection::establish(":memory:")?;
    // Dynamically-generated DDL; the table schema is ephemeral and has no
    // table! macro.
    for s in stmts.iter().filter(|s| !helpers::is_user_statement(s, "SELECT")) {
        diesel::sql_query(s.as_str()).execute(&mut conn)?;
    }
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
