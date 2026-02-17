//! Test for simple trigger CTE translation and execution.
//!
//! Scenario: When inserting into `orders`, automatically create a `log_entry`
//! record if one doesn't already exist for that order.

mod helpers;

use diesel::prelude::*;
use helpers::establish_connection;
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation},
    traits::TranslationOptions,
};
use rosetta_uuid::Uuid;

// Schema definitions for test tables
diesel::table! {
    /// Orders table for testing triggers.
    orders (id) {
        /// Order ID.
        id -> Binary,
        /// Customer name.
        customer_name -> Text,
    }
}

diesel::table! {
    /// Log entries table for tracking order operations.
    log_entries (id) {
        /// Log entry ID.
        id -> Binary,
        /// Associated order ID.
        order_id -> Binary,
        /// Log message.
        message -> Text,
    }
}

/// An order record.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = orders)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Order {
    /// Order ID.
    id: Vec<u8>,
    /// Customer name.
    customer_name: String,
}

/// Test a simple trigger with a single IF NOT EXISTS block.
#[test]
fn test_simple_trigger_if_not_exists() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
CREATE TABLE orders (id UUID PRIMARY KEY, customer_name TEXT NOT NULL);
CREATE TABLE log_entries (id UUID PRIMARY KEY, order_id UUID NOT NULL, message TEXT);

CREATE OR REPLACE FUNCTION log_new_order() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    v_log_id UUID;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM log_entries WHERE order_id = NEW.id
    ) THEN
        v_log_id := uuidv7();
        INSERT INTO log_entries (id, order_id, message) VALUES (v_log_id, NEW.id, 'Order created');
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER log_new_order_trigger
AFTER INSERT ON orders
FOR EACH ROW EXECUTE FUNCTION log_new_order();
";

    // Translate the SQL
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string());
    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    // Snapshot the translated SQL for consistency checking
    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("simple_trigger_if_not_exists", translated_sql);

    let mut connection = establish_connection();

    // Run the translations
    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    // Test: Insert an order - should automatically create a log entry
    let order_id = Uuid::new_v4();
    diesel::insert_into(orders::table)
        .values(&Order { id: order_id.as_bytes().to_vec(), customer_name: "Alice".to_string() })
        .execute(&mut connection)?;

    // Count log entries - should have 1
    let log_count: i64 = log_entries::table.count().get_result(&mut connection)?;
    assert_eq!(log_count, 1, "Expected 1 log entry");

    // Insert another order
    let order_id_2 = Uuid::new_v4();
    diesel::insert_into(orders::table)
        .values(&Order { id: order_id_2.as_bytes().to_vec(), customer_name: "Bob".to_string() })
        .execute(&mut connection)?;

    let log_count_2: i64 = log_entries::table.count().get_result(&mut connection)?;
    assert_eq!(log_count_2, 2, "Expected 2 log entries");

    Ok(())
}
