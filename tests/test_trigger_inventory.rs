//! Test for complex trigger with cascading effects.
//!
//! Scenario: E-commerce system with products, inventory, and audit trail.
//! - When a product is inserted, create an inventory record with initial stock
//! - When inventory changes, create an audit log entry

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

/// Test a complex trigger scenario with multiple tables and cascading effects.
#[test]
#[allow(clippy::too_many_lines)]
fn test_complex_trigger_with_multiple_variables() -> Result<(), Box<dyn std::error::Error>> {
    #[derive(QueryableByName, Debug)]
    struct InventoryQuantity {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        quantity: i32,
    }

    #[derive(QueryableByName, Debug)]
    struct AuditInfo {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
        old_quantity: Option<i32>,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        new_quantity: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        change_type: String,
    }

    let sql = r"
-- Products table
CREATE TABLE products (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    price REAL NOT NULL,
    initial_stock INTEGER NOT NULL DEFAULT 0
);

-- Inventory table (one record per product)
CREATE TABLE inventory (
    id UUID PRIMARY KEY,
    product_id UUID NOT NULL UNIQUE,
    quantity INTEGER NOT NULL DEFAULT 0,
    last_updated TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Audit log for tracking all inventory changes
CREATE TABLE inventory_audit (
    id UUID PRIMARY KEY,
    inventory_id UUID NOT NULL,
    product_id UUID NOT NULL,
    old_quantity INTEGER,
    new_quantity INTEGER NOT NULL,
    change_type TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Trigger: When a product is inserted, create inventory record if not exists
CREATE OR REPLACE FUNCTION create_product_inventory() RETURNS TRIGGER LANGUAGE plpgsql AS $$ 
DECLARE
    v_inventory_id UUID;
BEGIN 
    IF NOT EXISTS (
        SELECT 1 FROM inventory WHERE product_id = NEW.id
    ) THEN
        v_inventory_id := uuidv7();
        INSERT INTO inventory (id, product_id, quantity) 
        VALUES (v_inventory_id, NEW.id, NEW.initial_stock);
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER create_product_inventory_trigger
AFTER INSERT ON products
FOR EACH ROW EXECUTE FUNCTION create_product_inventory();

-- Trigger: When inventory is created, create audit record
CREATE OR REPLACE FUNCTION audit_inventory_creation() RETURNS TRIGGER LANGUAGE plpgsql AS $$ 
DECLARE
    v_audit_id UUID;
BEGIN 
    IF NOT EXISTS (
        SELECT 1 FROM inventory_audit WHERE inventory_id = NEW.id AND change_type = 'INITIAL'
    ) THEN
        v_audit_id := uuidv7();
        INSERT INTO inventory_audit (id, inventory_id, product_id, old_quantity, new_quantity, change_type)
        VALUES (v_audit_id, NEW.id, NEW.product_id, NULL, NEW.quantity, 'INITIAL');
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER audit_inventory_creation_trigger
AFTER INSERT ON inventory
FOR EACH ROW EXECUTE FUNCTION audit_inventory_creation();
";

    // Translate the SQL
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string());
    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    // Snapshot the translated SQL for consistency checking
    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("complex_trigger_with_multiple_variables", translated_sql);

    let mut connection = establish_connection();

    // Run the translations
    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    // Test 1: Insert a product with initial stock of 100
    let product_id_1 = uuid::Uuid::new_v4();
    diesel::sql_query(
        "INSERT INTO products (id, name, price, initial_stock) VALUES (?, 'Widget A', 19.99, 100)",
    )
    .bind::<diesel::sql_types::Binary, _>(product_id_1.as_bytes().to_vec())
    .execute(&mut connection)?;

    // Verify inventory was created
    let inventory_count: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM inventory").get_result(&mut connection)?;
    assert_eq!(inventory_count.count, 1, "Expected 1 inventory record");

    // Verify inventory quantity
    let inventory_qty: Vec<InventoryQuantity> =
        diesel::sql_query("SELECT quantity FROM inventory WHERE product_id = ?")
            .bind::<diesel::sql_types::Binary, _>(product_id_1.as_bytes().to_vec())
            .load(&mut connection)?;
    assert_eq!(inventory_qty.len(), 1);
    assert_eq!(inventory_qty[0].quantity, 100);

    // Verify audit log was created
    let audit_count: Count = diesel::sql_query("SELECT COUNT(*) as count FROM inventory_audit")
        .get_result(&mut connection)?;
    assert_eq!(audit_count.count, 1, "Expected 1 audit entry");

    // Verify audit details
    let audit_info: Vec<AuditInfo> = diesel::sql_query(
        "SELECT old_quantity, new_quantity, change_type FROM inventory_audit WHERE product_id = ?",
    )
    .bind::<diesel::sql_types::Binary, _>(product_id_1.as_bytes().to_vec())
    .load(&mut connection)?;
    assert_eq!(audit_info.len(), 1);
    assert_eq!(audit_info[0].old_quantity, None);
    assert_eq!(audit_info[0].new_quantity, 100);
    assert_eq!(audit_info[0].change_type, "INITIAL");

    // Test 2: Insert another product with different initial stock
    let product_id_2 = uuid::Uuid::new_v4();
    diesel::sql_query(
        "INSERT INTO products (id, name, price, initial_stock) VALUES (?, 'Gadget B', 49.99, 50)",
    )
    .bind::<diesel::sql_types::Binary, _>(product_id_2.as_bytes().to_vec())
    .execute(&mut connection)?;

    let inventory_count_2: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM inventory").get_result(&mut connection)?;
    assert_eq!(inventory_count_2.count, 2, "Expected 2 inventory records after second product");

    let audit_count_2: Count = diesel::sql_query("SELECT COUNT(*) as count FROM inventory_audit")
        .get_result(&mut connection)?;
    assert_eq!(audit_count_2.count, 2, "Expected 2 audit entries after second product");

    // Test 3: Verify that each product has its own inventory with correct quantity
    let inv_2_qty: Vec<InventoryQuantity> =
        diesel::sql_query("SELECT quantity FROM inventory WHERE product_id = ?")
            .bind::<diesel::sql_types::Binary, _>(product_id_2.as_bytes().to_vec())
            .load(&mut connection)?;
    assert_eq!(inv_2_qty.len(), 1);
    assert_eq!(inv_2_qty[0].quantity, 50);

    // Test 4: Verify the inventory_id in audit matches the actual inventory id
    let valid_audit_count: Count = diesel::sql_query(
        "SELECT COUNT(*) as count FROM inventory_audit a 
         WHERE EXISTS (SELECT 1 FROM inventory i WHERE i.id = a.inventory_id)",
    )
    .get_result(&mut connection)?;
    assert_eq!(
        valid_audit_count.count, 2,
        "All audit entries should reference valid inventory IDs"
    );

    Ok(())
}
