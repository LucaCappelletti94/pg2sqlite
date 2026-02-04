//! Test for trigger with conditional logic (IF NOT EXISTS).
//!
//! Scenario: Order processing system that creates notifications for new orders.

mod helpers;

use diesel::prelude::*;
use helpers::{Count, establish_connection};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation},
    traits::TranslationOptions,
};
use rosetta_uuid::Uuid;

/// Test trigger with IF NOT EXISTS for conditional execution.
#[test]
fn test_trigger_with_conditional_logic() -> Result<(), Box<dyn std::error::Error>> {
    #[derive(QueryableByName, Debug)]
    struct NotificationInfo {
        #[diesel(sql_type = diesel::sql_types::Text)]
        notification_type: String,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        priority: i32,
    }

    let sql = r"
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

    // Run the translations
    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    // Test 1: Insert a low-value order (should get ORDER_RECEIVED notification)
    let order_id_low = Uuid::new_v4();
    diesel::sql_query(
        "INSERT INTO customer_orders (id, customer_email, total_amount) VALUES (?, 'customer1@example.com', 50.00)",
    )
    .bind::<diesel::sql_types::Binary, _>(order_id_low.as_bytes().to_vec())
    .execute(&mut connection)?;

    let notifications_low: Vec<NotificationInfo> = diesel::sql_query(
        "SELECT notification_type, priority FROM notifications WHERE order_id = ?",
    )
    .bind::<diesel::sql_types::Binary, _>(order_id_low.as_bytes().to_vec())
    .load(&mut connection)?;

    assert_eq!(notifications_low.len(), 1, "Low-value order should have 1 notification");
    assert_eq!(notifications_low[0].notification_type, "ORDER_RECEIVED");
    assert_eq!(notifications_low[0].priority, 1);

    // Test 2: Insert another order (also gets notification)
    let order_id_high = Uuid::new_v4();
    diesel::sql_query(
        "INSERT INTO customer_orders (id, customer_email, total_amount) VALUES (?, 'customer2@example.com', 1500.00)",
    )
    .bind::<diesel::sql_types::Binary, _>(order_id_high.as_bytes().to_vec())
    .execute(&mut connection)?;

    let notifications_high: Vec<NotificationInfo> = diesel::sql_query(
        "SELECT notification_type, priority FROM notifications WHERE order_id = ? ORDER BY priority DESC",
    )
    .bind::<diesel::sql_types::Binary, _>(order_id_high.as_bytes().to_vec())
    .load(&mut connection)?;

    assert_eq!(notifications_high.len(), 1, "High-value order should have 1 notification");
    assert_eq!(notifications_high[0].notification_type, "ORDER_RECEIVED");

    // Test 3: Total notification count
    let total_count: Count = diesel::sql_query("SELECT COUNT(*) as count FROM notifications")
        .get_result(&mut connection)?;
    assert_eq!(total_count.count, 2, "Should have 2 total notifications");

    Ok(())
}
