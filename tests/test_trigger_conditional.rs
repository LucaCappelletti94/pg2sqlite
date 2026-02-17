//! Test for trigger with conditional logic (IF NOT EXISTS).
//!
//! Scenario: Order processing system that creates notifications for new orders.

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
    /// Customer orders table for testing conditional triggers.
    customer_orders (id) {
        /// Order ID.
        id -> Binary,
        /// Customer email address.
        customer_email -> Text,
        /// Total order amount.
        total_amount -> Double,
    }
}

diesel::table! {
    /// Notifications table for tracking order notifications.
    notifications (id) {
        /// Notification ID.
        id -> Binary,
        /// Associated order ID.
        order_id -> Binary,
        /// Type of notification.
        notification_type -> Text,
        /// Notification message.
        message -> Text,
        /// Notification priority level.
        priority -> Integer,
    }
}

diesel::allow_tables_to_appear_in_same_query!(customer_orders, notifications);

/// A customer order record.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = customer_orders)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct CustomerOrder {
    /// Order ID.
    id: Vec<u8>,
    /// Customer email address.
    customer_email: String,
    /// Total order amount.
    total_amount: f64,
}

/// A notification record.
#[derive(Queryable, Selectable)]
#[diesel(table_name = notifications)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Notification {
    /// Notification ID.
    #[allow(dead_code)]
    id: Vec<u8>,
    /// Associated order ID.
    #[allow(dead_code)]
    order_id: Vec<u8>,
    /// Type of notification.
    #[diesel(column_name = "notification_type")]
    kind: String,
    /// Notification message.
    #[allow(dead_code)]
    message: String,
    /// Notification priority level.
    priority: i32,
}

/// Test trigger with IF NOT EXISTS for conditional execution.
#[test]
fn test_trigger_with_conditional_logic() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
-- Orders table
CREATE TABLE customer_orders (
    id UUID PRIMARY KEY,
    customer_email TEXT NOT NULL,
    total_amount REAL NOT NULL
);

-- Notifications table
CREATE TABLE notifications (
    id UUID PRIMARY KEY,
    order_id UUID NOT NULL,
    notification_type TEXT NOT NULL,
    message TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0
);

-- Trigger: Create standard notification for all orders
CREATE OR REPLACE FUNCTION notify_new_order() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    v_notif_id UUID;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM notifications WHERE order_id = NEW.id AND notification_type = 'ORDER_RECEIVED'
    ) THEN
        v_notif_id := uuidv7();
        INSERT INTO notifications (id, order_id, notification_type, message, priority)
        VALUES (v_notif_id, NEW.id, 'ORDER_RECEIVED', 'Order confirmation sent', 1);
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER notify_new_order_trigger
AFTER INSERT ON customer_orders
FOR EACH ROW EXECUTE FUNCTION notify_new_order();
";

    // Translate the SQL
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string());
    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    // Snapshot the translated SQL for consistency checking
    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("conditional_trigger_if_not_exists", translated_sql);

    let mut connection = establish_connection();

    // Enable recursive triggers if needed
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;

    // Run the translations
    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    // Test 1: Insert a low-value order (should get ORDER_RECEIVED notification)
    let order_id_low = Uuid::new_v4();
    diesel::insert_into(customer_orders::table)
        .values(&CustomerOrder {
            id: order_id_low.as_bytes().to_vec(),
            customer_email: "customer1@example.com".to_string(),
            total_amount: 50.00,
        })
        .execute(&mut connection)?;

    let notifications_low: Vec<Notification> = notifications::table
        .filter(notifications::order_id.eq(order_id_low.as_bytes().to_vec()))
        .select(Notification::as_select())
        .load(&mut connection)?;

    assert_eq!(notifications_low.len(), 1, "Low-value order should have 1 notification");
    assert_eq!(notifications_low[0].kind, "ORDER_RECEIVED");
    assert_eq!(notifications_low[0].priority, 1);

    // Test 2: Insert another order (also gets notification)
    let order_id_high = Uuid::new_v4();
    diesel::insert_into(customer_orders::table)
        .values(&CustomerOrder {
            id: order_id_high.as_bytes().to_vec(),
            customer_email: "customer2@example.com".to_string(),
            total_amount: 1500.00,
        })
        .execute(&mut connection)?;

    let notifications_high: Vec<Notification> = notifications::table
        .filter(notifications::order_id.eq(order_id_high.as_bytes().to_vec()))
        .order(notifications::priority.desc())
        .select(Notification::as_select())
        .load(&mut connection)?;

    assert_eq!(notifications_high.len(), 1, "High-value order should have 1 notification");
    assert_eq!(notifications_high[0].kind, "ORDER_RECEIVED");

    // Test 3: Total notification count
    let total_count: i64 = notifications::table.count().get_result(&mut connection)?;
    assert_eq!(total_count, 2, "Should have 2 total notifications");

    Ok(())
}
