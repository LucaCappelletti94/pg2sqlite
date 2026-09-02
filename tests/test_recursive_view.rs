//! Tests for views with recursive CTEs.

#![allow(dead_code)]

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        /// Categories table for testing recursive views.
        categories (id) {
            /// Category ID.
            id -> Integer,
            /// Category name.
            name -> Text,
            /// Parent category ID (nullable for root categories).
            parent_id -> Nullable<Integer>,
        }
    }
}

use schema::categories;

/// A new category for insertion.
#[derive(Insertable)]
#[diesel(table_name = categories)]
struct NewCategory {
    /// Category name.
    name: String,
    /// Parent category ID (nullable for root categories).
    parent_id: Option<i32>,
}

/// Result from the category_tree recursive view.
#[derive(QueryableByName, Debug)]
struct CategoryTree {
    /// Category ID.
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    /// Category name.
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
    /// Parent category ID (nullable for root categories).
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    parent_id: Option<i32>,
    /// Depth in the category hierarchy (0 for root).
    #[diesel(sql_type = diesel::sql_types::Integer)]
    depth: i32,
    /// Full path from root to this category.
    #[diesel(sql_type = diesel::sql_types::Text)]
    path: String,
}

/// Result from the category_descendant_count view.
#[derive(QueryableByName, Debug)]
struct CategoryDescendantCount {
    /// Category ID.
    #[diesel(sql_type = diesel::sql_types::Integer)]
    category_id: i32,
    /// Number of descendant categories.
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    descendant_count: i64,
}

fn translate_and_setup() -> Result<diesel::SqliteConnection, Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/recursive_view.sql");

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

/// Snapshot test for recursive view translation.
#[test]
fn test_recursive_view_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/recursive_view.sql");

    let options = Pg2SqliteOptions::default();

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    insta::assert_snapshot!("recursive_view_translation", translated_sql);

    Ok(())
}

#[test]
fn test_category_tree_view() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // Insert a category hierarchy:
    // Electronics (root)
    //   ├── Computers
    //   │   ├── Laptops
    //   │   └── Desktops
    //   └── Phones
    // Books (root)

    diesel::insert_into(categories::table)
        .values(&[
            NewCategory { name: "Electronics".to_string(), parent_id: None },
            NewCategory { name: "Books".to_string(), parent_id: None },
        ])
        .execute(&mut connection)?;

    diesel::insert_into(categories::table)
        .values(&[
            NewCategory { name: "Computers".to_string(), parent_id: Some(1) },
            NewCategory { name: "Phones".to_string(), parent_id: Some(1) },
        ])
        .execute(&mut connection)?;

    diesel::insert_into(categories::table)
        .values(&[
            NewCategory { name: "Laptops".to_string(), parent_id: Some(3) },
            NewCategory { name: "Desktops".to_string(), parent_id: Some(3) },
        ])
        .execute(&mut connection)?;

    // Query the recursive view
    let tree = diesel::sql_query(
        "SELECT id, name, parent_id, depth, path FROM category_tree ORDER BY path",
    )
    .load::<CategoryTree>(&mut connection)?;

    // Should have 6 categories
    assert_eq!(tree.len(), 6, "Should have 6 categories in tree");

    // Check root categories have depth 0
    let roots: Vec<_> = tree.iter().filter(|c| c.depth == 0).collect();
    assert_eq!(roots.len(), 2, "Should have 2 root categories");

    // Check Electronics hierarchy
    let electronics = tree.iter().find(|c| c.name == "Electronics").unwrap();
    assert_eq!(electronics.depth, 0);
    assert_eq!(electronics.path, "Electronics");

    let computers = tree.iter().find(|c| c.name == "Computers").unwrap();
    assert_eq!(computers.depth, 1);
    assert_eq!(computers.path, "Electronics > Computers");

    let laptops = tree.iter().find(|c| c.name == "Laptops").unwrap();
    assert_eq!(laptops.depth, 2);
    assert_eq!(laptops.path, "Electronics > Computers > Laptops");

    Ok(())
}

#[test]
fn test_category_descendant_count_view() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // Same hierarchy as above
    diesel::insert_into(categories::table)
        .values(&[
            NewCategory { name: "Electronics".to_string(), parent_id: None },
            NewCategory { name: "Books".to_string(), parent_id: None },
        ])
        .execute(&mut connection)?;

    diesel::insert_into(categories::table)
        .values(&[
            NewCategory { name: "Computers".to_string(), parent_id: Some(1) },
            NewCategory { name: "Phones".to_string(), parent_id: Some(1) },
        ])
        .execute(&mut connection)?;

    diesel::insert_into(categories::table)
        .values(&[
            NewCategory { name: "Laptops".to_string(), parent_id: Some(3) },
            NewCategory { name: "Desktops".to_string(), parent_id: Some(3) },
        ])
        .execute(&mut connection)?;

    // Query the descendant count view
    let counts = diesel::sql_query(
        "SELECT category_id, descendant_count FROM category_descendant_count ORDER BY category_id",
    )
    .load::<CategoryDescendantCount>(&mut connection)?;

    assert_eq!(counts.len(), 6, "Should have counts for 6 categories");

    // Electronics (id=1) has 4 descendants: Computers, Phones, Laptops,
    // Desktops
    let electronics_count = counts.iter().find(|c| c.category_id == 1).unwrap();
    assert_eq!(electronics_count.descendant_count, 4);

    // Books (id=2) has 0 descendants
    let books_count = counts.iter().find(|c| c.category_id == 2).unwrap();
    assert_eq!(books_count.descendant_count, 0);

    // Computers (id=3) has 2 descendants: Laptops, Desktops
    let computers_count = counts.iter().find(|c| c.category_id == 3).unwrap();
    assert_eq!(computers_count.descendant_count, 2);

    // Laptops (id=5) has 0 descendants (leaf node)
    let laptops_count = counts.iter().find(|c| c.category_id == 5).unwrap();
    assert_eq!(laptops_count.descendant_count, 0);

    Ok(())
}

#[test]
fn test_recursive_view_empty_table() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // Query empty views
    let tree = diesel::sql_query("SELECT id, name, parent_id, depth, path FROM category_tree")
        .load::<CategoryTree>(&mut connection)?;

    assert_eq!(tree.len(), 0, "Empty table should return empty tree");

    let counts =
        diesel::sql_query("SELECT category_id, descendant_count FROM category_descendant_count")
            .load::<CategoryDescendantCount>(&mut connection)?;

    assert_eq!(counts.len(), 0, "Empty table should return no counts");

    Ok(())
}
