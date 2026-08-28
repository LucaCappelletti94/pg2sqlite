//! Test for cascading trigger translation across multiple tables.

mod helpers;

use diesel::prelude::*;
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};
use rosetta_uuid::Uuid;

// Schema definitions for test tables
diesel::table! {
    /// Products table for e-commerce system.
    products (id) {
        /// Product ID.
        id -> Binary,
        /// Product name.
        name -> Text,
        /// Product price.
        price -> Float,
        /// Initial stock quantity.
        initial_stock -> Integer,
    }
}

diesel::table! {
    /// Inventory table tracking stock levels.
    inventory (id) {
        /// Inventory record ID.
        id -> Binary,
        /// Associated product ID.
        product_id -> Binary,
        /// Current quantity in stock.
        quantity -> Integer,
        /// Last updated timestamp.
        last_updated -> Nullable<Text>,
    }
}

diesel::table! {
    /// Audit log for tracking inventory changes.
    inventory_audit (id) {
        /// Audit record ID.
        id -> Binary,
        /// Associated inventory ID.
        inventory_id -> Binary,
        /// Associated product ID.
        product_id -> Binary,
        /// Previous quantity (NULL for initial creation).
        old_quantity -> Nullable<Integer>,
        /// New quantity.
        new_quantity -> Integer,
        /// Type of change (INITIAL, UPDATE, etc.).
        change_type -> Text,
        /// Timestamp when change was recorded.
        created_at -> Nullable<Text>,
    }
}

diesel::allow_tables_to_appear_in_same_query!(products, inventory, inventory_audit);

/// A product in the e-commerce system.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = products)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Product {
    /// Product ID.
    id: Vec<u8>,
    /// Product name.
    name: String,
    /// Product price.
    price: f32,
    /// Initial stock quantity.
    initial_stock: i32,
}

/// An inventory record for a product.
#[derive(Queryable, Selectable)]
#[diesel(table_name = inventory)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
#[allow(dead_code)]
struct Inventory {
    /// Inventory record ID.
    id: Vec<u8>,
    /// Associated product ID.
    product_id: Vec<u8>,
    /// Current quantity in stock.
    quantity: i32,
    /// Last updated timestamp.
    last_updated: Option<String>,
}

/// An audit log entry for inventory changes.
#[derive(Queryable, Selectable)]
#[diesel(table_name = inventory_audit)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
#[allow(dead_code)]
struct InventoryAudit {
    /// Audit record ID.
    id: Vec<u8>,
    /// Associated inventory ID.
    inventory_id: Vec<u8>,
    /// Associated product ID.
    product_id: Vec<u8>,
    /// Previous quantity (NULL for initial creation).
    old_quantity: Option<i32>,
    /// New quantity.
    new_quantity: i32,
    /// Type of change (INITIAL, UPDATE, etc.).
    change_type: String,
    /// Timestamp when change was recorded.
    created_at: Option<String>,
}

#[test]
#[allow(clippy::too_many_lines)]
fn test_complex_trigger_with_multiple_variables() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
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
        .with_uuid_v7_function_name("uuidv7");
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
    let product_id_1 = Uuid::new_v4();
    diesel::insert_into(products::table)
        .values(&Product {
            id: product_id_1.as_bytes().to_vec(),
            name: "Widget A".to_string(),
            price: 19.99,
            initial_stock: 100,
        })
        .execute(&mut connection)?;

    // Verify inventory was created
    let inventory_count: i64 = inventory::table.count().get_result(&mut connection)?;
    assert_eq!(inventory_count, 1, "Expected 1 inventory record");

    // Verify inventory quantity
    let inventory_records: Vec<Inventory> = inventory::table
        .filter(inventory::product_id.eq(product_id_1.as_bytes().to_vec()))
        .select(Inventory::as_select())
        .load(&mut connection)?;
    assert_eq!(inventory_records.len(), 1);
    assert_eq!(inventory_records[0].quantity, 100);

    // Verify audit log was created
    let audit_count: i64 = inventory_audit::table.count().get_result(&mut connection)?;
    assert_eq!(audit_count, 1, "Expected 1 audit entry");

    // Verify audit details
    let audit_records: Vec<InventoryAudit> = inventory_audit::table
        .filter(inventory_audit::product_id.eq(product_id_1.as_bytes().to_vec()))
        .select(InventoryAudit::as_select())
        .load(&mut connection)?;
    assert_eq!(audit_records.len(), 1);
    assert_eq!(audit_records[0].old_quantity, None);
    assert_eq!(audit_records[0].new_quantity, 100);
    assert_eq!(audit_records[0].change_type, "INITIAL");

    // Test 2: Insert another product with different initial stock
    let product_id_2 = Uuid::new_v4();
    diesel::insert_into(products::table)
        .values(&Product {
            id: product_id_2.as_bytes().to_vec(),
            name: "Gadget B".to_string(),
            price: 49.99,
            initial_stock: 50,
        })
        .execute(&mut connection)?;

    let inventory_count_2: i64 = inventory::table.count().get_result(&mut connection)?;
    assert_eq!(inventory_count_2, 2, "Expected 2 inventory records after second product");

    let audit_count_2: i64 = inventory_audit::table.count().get_result(&mut connection)?;
    assert_eq!(audit_count_2, 2, "Expected 2 audit entries after second product");

    // Test 3: Verify that each product has its own inventory with correct quantity
    let inv_2_records: Vec<Inventory> = inventory::table
        .filter(inventory::product_id.eq(product_id_2.as_bytes().to_vec()))
        .select(Inventory::as_select())
        .load(&mut connection)?;
    assert_eq!(inv_2_records.len(), 1);
    assert_eq!(inv_2_records[0].quantity, 50);

    // Test 4: Verify the inventory_id in audit matches the actual inventory id
    // Use diesel to load all inventory IDs and audit inventory IDs, then verify in
    // Rust
    let inventory_ids: Vec<Vec<u8>> =
        inventory::table.select(inventory::id).load(&mut connection)?;
    let audit_inventory_ids: Vec<Vec<u8>> =
        inventory_audit::table.select(inventory_audit::inventory_id).load(&mut connection)?;

    let valid_audit_count = audit_inventory_ids
        .iter()
        .filter(|audit_inv_id| inventory_ids.contains(audit_inv_id))
        .count();

    assert_eq!(valid_audit_count, 2, "All audit entries should reference valid inventory IDs");

    Ok(())
}
