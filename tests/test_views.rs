//! Tests for CREATE VIEW translation.

#![allow(dead_code)]

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        users (id) {
            id -> Integer,
            username -> Text,
            email -> Text,
            is_active -> Integer,
        }
    }

    diesel::table! {
        orders (id) {
            id -> Integer,
            user_id -> Integer,
            total -> Float,
            status -> Text,
        }
    }
}

use schema::{orders, users};

#[derive(Insertable)]
#[diesel(table_name = users)]
struct NewUser {
    username: String,
    email: String,
    is_active: i32,
}

#[derive(Insertable)]
#[diesel(table_name = orders)]
struct NewOrder {
    user_id: i32,
    total: f32,
    status: String,
}

#[derive(QueryableByName, Debug)]
struct ActiveUser {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    username: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    email: String,
}

#[derive(QueryableByName, Debug)]
struct UserOrder {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    user_id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    username: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    order_id: i32,
    #[diesel(sql_type = diesel::sql_types::Float)]
    total: f32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    status: String,
}

#[derive(QueryableByName, Debug)]
struct UserOrderSummary {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    username: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    order_count: i64,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Float>)]
    total_spent: Option<f32>,
}

fn translate_and_setup() -> Result<diesel::SqliteConnection, Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/views.sql");

    let options = Pg2SqliteOptions::default();

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection =
        diesel::SqliteConnection::establish(":memory:").expect("Failed to connect");

    diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut connection)?;

    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    Ok(connection)
}

/// Snapshot test for views translation.
#[test]
fn test_views_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/views.sql");

    let options = Pg2SqliteOptions::default();

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    insta::assert_snapshot!("views_translation", translated_sql);

    Ok(())
}

/// Test that simple view works.
#[test]
fn test_simple_view_active_users() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // Insert users - one active, one inactive
    diesel::insert_into(users::table)
        .values(&[
            NewUser {
                username: "alice".to_string(),
                email: "alice@example.com".to_string(),
                is_active: 1,
            },
            NewUser {
                username: "bob".to_string(),
                email: "bob@example.com".to_string(),
                is_active: 0,
            },
        ])
        .execute(&mut connection)?;

    // Query the view
    let active = diesel::sql_query("SELECT id, username, email FROM active_users")
        .load::<ActiveUser>(&mut connection)?;

    assert_eq!(active.len(), 1);
    assert_eq!(active[0].username, "alice");

    Ok(())
}

/// Test view with join.
#[test]
fn test_view_with_join() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // Insert a user
    diesel::insert_into(users::table)
        .values(NewUser {
            username: "alice".to_string(),
            email: "alice@example.com".to_string(),
            is_active: 1,
        })
        .execute(&mut connection)?;

    // Insert orders for the user
    diesel::insert_into(orders::table)
        .values(&[
            NewOrder { user_id: 1, total: 100.0, status: "completed".to_string() },
            NewOrder { user_id: 1, total: 50.0, status: "pending".to_string() },
        ])
        .execute(&mut connection)?;

    // Query the view
    let user_orders =
        diesel::sql_query("SELECT user_id, username, order_id, total, status FROM user_orders")
            .load::<UserOrder>(&mut connection)?;

    assert_eq!(user_orders.len(), 2);
    assert_eq!(user_orders[0].username, "alice");

    Ok(())
}

/// Test view with aggregation.
#[test]
fn test_view_with_aggregation() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // Insert users
    diesel::insert_into(users::table)
        .values(&[
            NewUser {
                username: "alice".to_string(),
                email: "alice@example.com".to_string(),
                is_active: 1,
            },
            NewUser {
                username: "bob".to_string(),
                email: "bob@example.com".to_string(),
                is_active: 1,
            },
        ])
        .execute(&mut connection)?;

    // Insert orders for alice only
    diesel::insert_into(orders::table)
        .values(&[
            NewOrder { user_id: 1, total: 100.0, status: "completed".to_string() },
            NewOrder { user_id: 1, total: 50.0, status: "completed".to_string() },
        ])
        .execute(&mut connection)?;

    // Query the view
    let summaries = diesel::sql_query(
        "SELECT id, username, order_count, total_spent FROM user_order_summary ORDER BY id",
    )
    .load::<UserOrderSummary>(&mut connection)?;

    assert_eq!(summaries.len(), 2);

    // Alice has 2 orders totaling 150
    assert_eq!(summaries[0].username, "alice");
    assert_eq!(summaries[0].order_count, 2);
    assert!((summaries[0].total_spent.unwrap() - 150.0).abs() < 0.01);

    // Bob has 0 orders
    assert_eq!(summaries[1].username, "bob");
    assert_eq!(summaries[1].order_count, 0);
    assert!(summaries[1].total_spent.is_none());

    Ok(())
}

/// Test that materialized views cause an error.
#[test]
fn test_materialized_view_error() {
    let sql = "CREATE MATERIALIZED VIEW order_stats AS SELECT status FROM orders;";

    let options = Pg2SqliteOptions::default();

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("MATERIALIZED VIEW"),
        "Error should mention MATERIALIZED VIEW: {}",
        err
    );
}

/// Test that OR REPLACE VIEW causes an error.
#[test]
fn test_or_replace_view_error() {
    let sql = "CREATE OR REPLACE VIEW my_view AS SELECT 1;";

    let options = Pg2SqliteOptions::default();

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("OR REPLACE VIEW"),
        "Error should mention OR REPLACE VIEW: {}",
        err
    );
}
