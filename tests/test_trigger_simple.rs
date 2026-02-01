//! Test for simple trigger CTE translation and execution.
//!
//! Scenario: When inserting into `orders`, automatically create a `log_entry`
//! record if one doesn't already exist for that order.

mod helpers;

use diesel::{prelude::*, sqlite::SqliteConnection};
use helpers::{Count, establish_connection_base, uuidv7_impl};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation},
    traits::TranslationOptions,
};

#[declare_sql_function]
extern "SQL" {
    /// Generates a UUIDv7 value.
    fn uuidv7() -> diesel::sql_types::Binary;
}

fn establish_connection() -> SqliteConnection {
    let connection = establish_connection_base();
    uuidv7_utils::register_impl(&connection, uuidv7_impl).expect("Failed to register uuidv7");
    connection
}

/// Test a simple trigger with a single IF NOT EXISTS block.
#[test]
fn test_simple_trigger_if_not_exists() -> Result<(), Box<dyn std::error::Error>> {
    let sql = r"
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
    let order_id = uuid::Uuid::new_v4();
    diesel::sql_query("INSERT INTO orders (id, customer_name) VALUES (?, 'Alice')")
        .bind::<diesel::sql_types::Binary, _>(order_id.as_bytes().to_vec())
        .execute(&mut connection)?;

    // Count log entries - should have 1
    let log_count: Count = diesel::sql_query("SELECT COUNT(*) as count FROM log_entries")
        .get_result(&mut connection)?;
    assert_eq!(log_count.count, 1, "Expected 1 log entry");

    // Insert another order
    let order_id_2 = uuid::Uuid::new_v4();
    diesel::sql_query("INSERT INTO orders (id, customer_name) VALUES (?, 'Bob')")
        .bind::<diesel::sql_types::Binary, _>(order_id_2.as_bytes().to_vec())
        .execute(&mut connection)?;

    let log_count_2: Count = diesel::sql_query("SELECT COUNT(*) as count FROM log_entries")
        .get_result(&mut connection)?;
    assert_eq!(log_count_2.count, 2, "Expected 2 log entries");

    Ok(())
}
