//! Test for groups with recursive membership propagation.
//!
//! This test verifies:
//! 1. The PostgreSQL schema with recursive CTEs in triggers translates to
//!    SQLite
//! 2. The translated schema works with Diesel ORM
//! 3. Recursive membership propagation works correctly

#![allow(clippy::missing_errors_doc)]

mod helpers;

use diesel::{prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation},
    traits::TranslationOptions,
};
use rosetta_uuid::Uuid;

diesel::table! {
    use diesel::sql_types::*;
    use rosetta_uuid::diesel_impls::Uuid;

    /// Groups table with hierarchy via parent_group_id.
    groups (id) {
        /// Primary key
        id -> Uuid,
        /// Optional parent group for hierarchy.
        parent_group_id -> Nullable<Uuid>,
        /// Group name.
        name -> Text,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use rosetta_uuid::diesel_impls::Uuid;

    /// Users table.
    users (id) {
        /// Primary key (UUIDv7 as BLOB).
        id -> Uuid,
        /// User name.
        name -> Text,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use rosetta_uuid::diesel_impls::Uuid;

    /// Junction table for group memberships.
    group_memberships (id) {
        /// Primary key.
        id -> Uuid,
        /// The group.
        group_id -> Uuid,
        /// The user.
        user_id -> Uuid,
    }
}

diesel::joinable!(group_memberships -> groups (group_id));
diesel::joinable!(group_memberships -> users (user_id));

diesel::allow_tables_to_appear_in_same_query!(groups, users, group_memberships,);

/// A group entity with optional parent for hierarchy.
#[derive(Queryable, Selectable, Debug, Insertable, PartialEq)]
#[diesel(table_name = groups)]
pub struct Group {
    /// Primary key.
    pub id: Uuid,
    /// Optional parent group for hierarchy.
    parent_group_id: Option<Uuid>,
    /// Group name.
    name: String,
}

impl Group {
    /// Creates a new group with the provided name and optional parent.
    #[must_use]
    pub fn new(name: &str, parent_group: Option<&Group>) -> Self {
        Self {
            id: Uuid::utc_v7(),
            parent_group_id: parent_group.map(|g| g.id),
            name: name.to_string(),
        }
    }

    /// Inserts the group into the database.
    pub fn insert(self, conn: &mut SqliteConnection) -> Result<Self, Box<dyn std::error::Error>> {
        diesel::insert_into(groups::table).values(&self).execute(conn)?;
        assert!(self.exists(conn)?);
        Ok(self)
    }

    /// Returns whether the group exists in the database.
    pub fn exists(&self, conn: &mut SqliteConnection) -> Result<bool, Box<dyn std::error::Error>> {
        let count: i64 = groups::table.filter(groups::id.eq(&self.id)).count().get_result(conn)?;
        Ok(count > 0)
    }

    /// Adds the provided user as a member of this group.
    pub fn add_member(
        &self,
        user: &User,
        conn: &mut SqliteConnection,
    ) -> Result<GroupMembership, Box<dyn std::error::Error>> {
        GroupMembership::new(self, user).insert(conn)
    }

    /// Returns whether the group has the specified user as a member.
    pub fn has_member(
        &self,
        user: &User,
        conn: &mut SqliteConnection,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let count: i64 = group_memberships::table
            .filter(group_memberships::group_id.eq(&self.id))
            .filter(group_memberships::user_id.eq(&user.id))
            .count()
            .get_result(conn)?;
        Ok(count > 0)
    }

    /// Deletes the group from the database.
    pub fn delete(&self, conn: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
        diesel::delete(groups::table.filter(groups::id.eq(&self.id))).execute(conn)?;
        Ok(())
    }

    /// Removes the provided user from this group.
    pub fn remove_member(
        &self,
        user: &User,
        conn: &mut SqliteConnection,
    ) -> Result<(), Box<dyn std::error::Error>> {
        diesel::delete(
            group_memberships::table
                .filter(group_memberships::group_id.eq(&self.id))
                .filter(group_memberships::user_id.eq(&user.id)),
        )
        .execute(conn)?;
        Ok(())
    }
}

/// A user entity.
#[derive(Queryable, Selectable, Debug, Insertable, PartialEq)]
#[diesel(table_name = users)]
pub struct User {
    /// Primary key.
    pub id: Uuid,
    /// User name.
    name: String,
}

impl User {
    /// Creates a new user with the provided name.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self { id: Uuid::utc_v7(), name: name.to_string() }
    }

    /// Inserts the user into the database.
    pub fn insert(self, conn: &mut SqliteConnection) -> Result<Self, Box<dyn std::error::Error>> {
        diesel::insert_into(users::table).values(&self).execute(conn)?;
        assert!(self.exists(conn)?);
        Ok(self)
    }

    /// Returns whether the user exists in the database.
    pub fn exists(&self, conn: &mut SqliteConnection) -> Result<bool, Box<dyn std::error::Error>> {
        let count: i64 = users::table.filter(users::id.eq(&self.id)).count().get_result(conn)?;
        Ok(count > 0)
    }

    /// Returns the memberships of this user.
    pub fn memberships(
        &self,
        conn: &mut SqliteConnection,
    ) -> Result<Vec<GroupMembership>, Box<dyn std::error::Error>> {
        let memberships: Vec<GroupMembership> =
            group_memberships::table.filter(group_memberships::user_id.eq(&self.id)).load(conn)?;
        Ok(memberships)
    }

    /// Returns the number of group memberships for this user.
    pub fn membership_count(
        &self,
        conn: &mut SqliteConnection,
    ) -> Result<i64, Box<dyn std::error::Error>> {
        let count: i64 = group_memberships::table
            .filter(group_memberships::user_id.eq(&self.id))
            .count()
            .get_result(conn)?;
        Ok(count)
    }
}

/// A group membership linking users to groups.
#[derive(Queryable, Selectable, Debug, Insertable, PartialEq)]
#[diesel(table_name = group_memberships)]
pub struct GroupMembership {
    /// Primary key.
    id: Uuid,
    /// The group being a member of.
    group_id: Uuid,
    /// The user.
    user_id: Uuid,
}

impl GroupMembership {
    /// Creates a new group membership.
    #[must_use]
    pub fn new(group: &Group, user: &User) -> Self {
        Self { id: Uuid::utc_v7(), group_id: group.id, user_id: user.id }
    }

    /// Inserts the group membership into the database.
    pub fn insert(self, conn: &mut SqliteConnection) -> Result<Self, Box<dyn std::error::Error>> {
        diesel::insert_into(group_memberships::table).values(&self).execute(conn)?;
        assert!(self.exists(conn)?);
        Ok(self)
    }

    /// Returns whether the group membership exists in the database.
    pub fn exists(&self, conn: &mut SqliteConnection) -> Result<bool, Box<dyn std::error::Error>> {
        let count: i64 = group_memberships::table
            .filter(group_memberships::id.eq(&self.id))
            .count()
            .get_result(conn)?;
        Ok(count > 0)
    }
}

fn setup_database(connection: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/groups.sql");
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string());
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(connection)?;
    }
    Ok(())
}

#[test]
fn test_groups_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/groups.sql");
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string());
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    // Join all statements for snapshot
    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    // Verify each statement is valid SQLite
    for stmt in &translated {
        let sql_str = stmt.to_string();
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::SQLiteDialect {}, &sql_str)
            .expect("Failed to parse translated SQL as SQLite");
    }

    insta::assert_snapshot!("groups_translation", translated_sql);
    Ok(())
}

#[test]
fn test_groups_schema_execution() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = establish_connection();
    setup_database(&mut connection)?;

    // Verify tables exist by counting rows (should be 0)
    let group_count: i64 = groups::table.count().get_result(&mut connection)?;
    assert_eq!(group_count, 0);

    let user_count: i64 = users::table.count().get_result(&mut connection)?;
    assert_eq!(user_count, 0);

    Ok(())
}

#[test]
fn test_diesel_crud_operations() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = establish_connection();
    setup_database(&mut connection)?;

    // Create a user
    let alice = User::new("Alice").insert(&mut connection)?;
    assert!(alice.exists(&mut connection)?);

    // Create a group
    let engineering = Group::new("Engineering", None).insert(&mut connection)?;
    assert!(engineering.exists(&mut connection)?);

    Ok(())
}

/// Test recursive membership propagation to parent groups.
/// When a member is added to a child group, they should automatically
/// be added to all parent groups.
#[test]
fn test_membership_propagates_to_parents() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = establish_connection();
    setup_database(&mut connection)?;

    // Create a member: Alice
    let alice = User::new("Alice").insert(&mut connection)?;

    // Create hierarchy: Company > Engineering > Backend
    let company = Group::new("Company", None).insert(&mut connection)?;
    let engineering = Group::new("Engineering", Some(&company)).insert(&mut connection)?;
    let backend = Group::new("Backend", Some(&engineering)).insert(&mut connection)?;

    let alice_membership = backend.add_member(&alice, &mut connection)?;
    let alice_memberships: Vec<GroupMembership> = alice.memberships(&mut connection)?;

    assert_eq!(
        alice_memberships.len(),
        3,
        "Alice should be in 3 groups (Backend, Engineering, Company)"
    );
    assert!(alice_memberships.contains(&alice_membership));

    // Verify specific group memberships
    assert!(company.has_member(&alice, &mut connection)?, "Alice should be a member of Company");
    assert!(
        engineering.has_member(&alice, &mut connection)?,
        "Alice should be a member of Engineering"
    );
    assert!(backend.has_member(&alice, &mut connection)?, "Alice should be a member of Backend");
    Ok(())
}

/// Test that removing a member from a parent group removes them from child
/// groups.
#[test]
fn test_membership_removal_cascades_to_children() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = establish_connection();
    setup_database(&mut connection)?;

    // Create a member: Bob
    let bob = User::new("Bob").insert(&mut connection)?;

    // Create hierarchy: Company > Engineering > Backend
    let company = Group::new("Company", None).insert(&mut connection)?;
    let engineering = Group::new("Engineering", Some(&company)).insert(&mut connection)?;
    let backend = Group::new("Backend", Some(&engineering)).insert(&mut connection)?;

    // Add Bob to Backend (will auto-propagate to Engineering and Company)
    backend.add_member(&bob, &mut connection)?;

    // Verify Bob is in 3 groups
    assert_eq!(bob.membership_count(&mut connection)?, 3);

    // Now remove Bob from Engineering (should also remove from Backend, but keep
    // Company)
    engineering.remove_member(&bob, &mut connection)?;

    // Verify Bob is only in Company now
    let bob_memberships = bob.memberships(&mut connection)?;
    assert_eq!(
        bob_memberships.len(),
        1,
        "Bob should only be in Company after removal from Engineering"
    );
    assert!(company.has_member(&bob, &mut connection)?, "Bob should remain in Company");
    assert!(!engineering.has_member(&bob, &mut connection)?, "Bob should not be in Engineering");
    assert!(!backend.has_member(&bob, &mut connection)?, "Bob should not be in Backend");

    Ok(())
}

#[test]
fn test_duplicate_membership_prevented() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = establish_connection();
    setup_database(&mut connection)?;

    // Create a user and a group
    let user = User::new("User").insert(&mut connection)?;
    let group = Group::new("Group", None).insert(&mut connection)?;

    // Add user to group
    group.add_member(&user, &mut connection)?;

    // Try to add the same membership again - should fail due to UNIQUE constraint
    let result = group.add_member(&user, &mut connection);

    assert!(result.is_err(), "Duplicate membership should be prevented by UNIQUE constraint");

    Ok(())
}

#[test]
fn test_cascade_delete_group_removes_memberships() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = establish_connection();
    setup_database(&mut connection)?;

    // Create a user and a group
    let user = User::new("Member").insert(&mut connection)?;
    let group = Group::new("ToDelete", None).insert(&mut connection)?;

    // Add user to group
    group.add_member(&user, &mut connection)?;

    // Verify membership exists
    assert_eq!(user.membership_count(&mut connection)?, 1);

    // Delete the group
    group.delete(&mut connection)?;

    // Memberships should be cascade deleted
    assert_eq!(
        user.membership_count(&mut connection)?,
        0,
        "Memberships should be cascade deleted with group"
    );

    Ok(())
}

#[test]
fn test_cascade_delete_parent_group_removes_children() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = establish_connection();
    setup_database(&mut connection)?;

    // Create parent and child groups
    let parent = Group::new("Parent", None).insert(&mut connection)?;
    let _child = Group::new("Child", Some(&parent)).insert(&mut connection)?;

    // Verify both groups exist
    let group_count: i64 = groups::table.count().get_result(&mut connection)?;
    assert_eq!(group_count, 2);

    // Delete parent group
    parent.delete(&mut connection)?;

    // Child should be cascade deleted
    let group_count_after: i64 = groups::table.count().get_result(&mut connection)?;
    assert_eq!(group_count_after, 0, "Child group should be cascade deleted with parent");

    Ok(())
}
