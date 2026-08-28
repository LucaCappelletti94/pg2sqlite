//! Test for RLS policies with public/unrestricted access patterns.
//!
//! Scenario: Some tables have public access (USING (true)), while others
//! restrict access based on ownership or featured status.

mod helpers;

use diesel::prelude::*;
use helpers::{establish_connection, set_session_user_id};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};
use rosetta_uuid::Uuid;

mod schema {
    diesel::table! {
        /// Categories view - publicly accessible to all.
        categories (id) {
            id -> Binary,
            name -> Text,
        }
    }

    diesel::table! {
        /// Items view with RLS filtering based on owner or featured status.
        items (id) {
            id -> Binary,
            category_id -> Nullable<Binary>,
            name -> Text,
            owner_id -> Binary,
            is_featured -> Bool,
        }
    }
}

use schema::{categories, items};

#[derive(Queryable, Insertable, Selectable, Debug, Clone)]
#[diesel(table_name = categories)]
struct Category {
    id: Vec<u8>,
    name: String,
}

#[derive(Queryable, Insertable, Selectable, Debug, Clone)]
#[diesel(table_name = items)]
struct Item {
    id: Vec<u8>,
    category_id: Option<Vec<u8>>,
    name: String,
    owner_id: Vec<u8>,
    is_featured: bool,
}

fn translate_and_setup() -> Result<diesel::SqliteConnection, Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_public_access.sql");

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection = establish_connection();

    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    Ok(connection)
}

/// Snapshot test for public access RLS translation.
#[test]
fn test_public_access_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_public_access.sql");

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("rls_public_access_translation", translated_sql);

    Ok(())
}

#[test]
fn test_public_table_visible_to_all() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // User Alice inserts a category
    let alice = Uuid::new_v4();
    set_session_user_id(&alice);
    diesel::insert_into(categories::table)
        .values(Category {
            id: Uuid::new_v4().as_bytes().to_vec(),
            name: "Electronics".to_string(),
        })
        .execute(&mut connection)?;

    // User Bob should also see it (public access)
    let bob = Uuid::new_v4();
    set_session_user_id(&bob);
    let visible_categories =
        categories::table.select(Category::as_select()).load(&mut connection)?;
    assert_eq!(visible_categories.len(), 1, "Bob should see the category");
    assert_eq!(visible_categories[0].name, "Electronics");

    Ok(())
}

#[test]
fn test_featured_items_visible_to_all() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();

    // Alice creates a featured item
    set_session_user_id(&alice);
    diesel::insert_into(items::table)
        .values(Item {
            id: Uuid::new_v4().as_bytes().to_vec(),
            category_id: None,
            name: "Featured Widget".to_string(),
            owner_id: alice.as_bytes().to_vec(),
            is_featured: true,
        })
        .execute(&mut connection)?;

    // Bob should see the featured item
    set_session_user_id(&bob);
    let visible_items = items::table.select(Item::as_select()).load(&mut connection)?;
    assert_eq!(visible_items.len(), 1, "Bob should see 1 featured item");
    assert_eq!(visible_items[0].name, "Featured Widget");

    Ok(())
}

#[test]
fn test_non_featured_visible_only_to_owner() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();

    // Alice creates a non-featured item
    set_session_user_id(&alice);
    diesel::insert_into(items::table)
        .values(Item {
            id: Uuid::new_v4().as_bytes().to_vec(),
            category_id: None,
            name: "Alice's Private Item".to_string(),
            owner_id: alice.as_bytes().to_vec(),
            is_featured: false,
        })
        .execute(&mut connection)?;

    // Alice should see her item
    let alice_items = items::table.select(Item::as_select()).load(&mut connection)?;
    assert_eq!(alice_items.len(), 1, "Alice should see 1 item");

    // Bob should NOT see Alice's non-featured item
    set_session_user_id(&bob);
    let bob_items = items::table.select(Item::as_select()).load(&mut connection)?;
    assert_eq!(bob_items.len(), 0, "Bob should see 0 items");

    Ok(())
}

#[test]
fn test_mixed_visibility_query() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();

    // Alice creates 2 items: one featured, one not
    set_session_user_id(&alice);
    diesel::insert_into(items::table)
        .values(&[
            Item {
                id: Uuid::new_v4().as_bytes().to_vec(),
                category_id: None,
                name: "Alice Featured".to_string(),
                owner_id: alice.as_bytes().to_vec(),
                is_featured: true,
            },
            Item {
                id: Uuid::new_v4().as_bytes().to_vec(),
                category_id: None,
                name: "Alice Private".to_string(),
                owner_id: alice.as_bytes().to_vec(),
                is_featured: false,
            },
        ])
        .execute(&mut connection)?;

    // Bob creates 2 items: one featured, one not
    set_session_user_id(&bob);
    diesel::insert_into(items::table)
        .values(&[
            Item {
                id: Uuid::new_v4().as_bytes().to_vec(),
                category_id: None,
                name: "Bob Featured".to_string(),
                owner_id: bob.as_bytes().to_vec(),
                is_featured: true,
            },
            Item {
                id: Uuid::new_v4().as_bytes().to_vec(),
                category_id: None,
                name: "Bob Private".to_string(),
                owner_id: bob.as_bytes().to_vec(),
                is_featured: false,
            },
        ])
        .execute(&mut connection)?;

    // Alice should see: Alice Featured, Alice Private, Bob Featured (3 items)
    set_session_user_id(&alice);
    let alice_visible = items::table.select(Item::as_select()).load(&mut connection)?;
    assert_eq!(alice_visible.len(), 3, "Alice should see 3 items");
    let alice_names: Vec<_> = alice_visible.iter().map(|i| i.name.as_str()).collect();
    assert!(alice_names.contains(&"Alice Featured"));
    assert!(alice_names.contains(&"Alice Private"));
    assert!(alice_names.contains(&"Bob Featured"));
    assert!(!alice_names.contains(&"Bob Private"));

    // Bob should see: Alice Featured, Bob Featured, Bob Private (3 items)
    set_session_user_id(&bob);
    let bob_visible = items::table.select(Item::as_select()).load(&mut connection)?;
    assert_eq!(bob_visible.len(), 3, "Bob should see 3 items");
    let bob_names: Vec<_> = bob_visible.iter().map(|i| i.name.as_str()).collect();
    assert!(bob_names.contains(&"Alice Featured"));
    assert!(bob_names.contains(&"Bob Featured"));
    assert!(bob_names.contains(&"Bob Private"));
    assert!(!bob_names.contains(&"Alice Private"));

    Ok(())
}

#[test]
fn test_owner_can_update_own_items() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice = Uuid::new_v4();
    let item_id = Uuid::new_v4();

    set_session_user_id(&alice);
    diesel::insert_into(items::table)
        .values(Item {
            id: item_id.as_bytes().to_vec(),
            category_id: None,
            name: "Original Name".to_string(),
            owner_id: alice.as_bytes().to_vec(),
            is_featured: false,
        })
        .execute(&mut connection)?;

    // Alice updates her item
    diesel::update(items::table.filter(items::id.eq(item_id.as_bytes().to_vec())))
        .set(items::name.eq("Updated Name"))
        .execute(&mut connection)?;

    let updated_item = items::table
        .filter(items::id.eq(item_id.as_bytes().to_vec()))
        .select(Item::as_select())
        .first(&mut connection)?;
    assert_eq!(updated_item.name, "Updated Name");

    Ok(())
}

#[test]
fn test_non_owner_cannot_update() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();
    let item_id = Uuid::new_v4();

    // Alice creates a featured item
    set_session_user_id(&alice);
    diesel::insert_into(items::table)
        .values(Item {
            id: item_id.as_bytes().to_vec(),
            category_id: None,
            name: "Alice's Featured".to_string(),
            owner_id: alice.as_bytes().to_vec(),
            is_featured: true,
        })
        .execute(&mut connection)?;

    // Bob tries to update (should affect 0 rows due to USING clause)
    set_session_user_id(&bob);
    diesel::update(items::table.filter(items::id.eq(item_id.as_bytes().to_vec())))
        .set(items::name.eq("Hacked"))
        .execute(&mut connection)?;

    // Verify the item was NOT changed
    set_session_user_id(&alice);
    let item = items::table
        .filter(items::id.eq(item_id.as_bytes().to_vec()))
        .select(Item::as_select())
        .first(&mut connection)?;
    assert_eq!(item.name, "Alice's Featured", "Item should NOT be updated by Bob");

    Ok(())
}

#[test]
fn test_owner_can_delete_own_items() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice = Uuid::new_v4();
    let item_id = Uuid::new_v4();

    set_session_user_id(&alice);
    diesel::insert_into(items::table)
        .values(Item {
            id: item_id.as_bytes().to_vec(),
            category_id: None,
            name: "To Delete".to_string(),
            owner_id: alice.as_bytes().to_vec(),
            is_featured: false,
        })
        .execute(&mut connection)?;

    // Alice deletes her item
    diesel::delete(items::table.filter(items::id.eq(item_id.as_bytes().to_vec())))
        .execute(&mut connection)?;

    let count = items::table.count().get_result::<i64>(&mut connection)?;
    assert_eq!(count, 0, "Item should be deleted");

    Ok(())
}

#[test]
fn test_non_owner_cannot_delete() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();
    let item_id = Uuid::new_v4();

    // Alice creates a featured item
    set_session_user_id(&alice);
    diesel::insert_into(items::table)
        .values(Item {
            id: item_id.as_bytes().to_vec(),
            category_id: None,
            name: "Alice's Featured".to_string(),
            owner_id: alice.as_bytes().to_vec(),
            is_featured: true,
        })
        .execute(&mut connection)?;

    // Bob tries to delete (should affect 0 rows due to USING clause)
    set_session_user_id(&bob);
    diesel::delete(items::table.filter(items::id.eq(item_id.as_bytes().to_vec())))
        .execute(&mut connection)?;

    // Verify the item still exists
    set_session_user_id(&alice);
    let count = items::table.count().get_result::<i64>(&mut connection)?;
    assert_eq!(count, 1, "Item should NOT be deleted by Bob");

    Ok(())
}
