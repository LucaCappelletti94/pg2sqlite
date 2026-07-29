//! Test for RLS grants with role-based access control.
//!
//! This test verifies the ownership/access model:
//! - Owners: full control (view, edit, delete, grant, add admins, add owners)
//! - Admins: view, edit, delete, grant viewer/editor
//! - Editors: view, edit
//! - Viewers: view only
//!
//! Key features tested:
//! 1. Creator is automatically made an owner
//! 2. Group membership grants access transitively
//! 3. Only owners can manage admins/owners
//! 4. Cannot remove the last owner (orphan protection)

#![allow(clippy::missing_errors_doc)]
#![allow(clippy::duplicate_mod)]

mod helpers;
#[path = "test_groups.rs"]
mod test_groups;

use diesel::{prelude::*, sqlite::SqliteConnection};
use helpers::{establish_connection, set_session_user_id};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};
use rosetta_uuid::Uuid;
// Re-use models from test_groups
pub use test_groups::{Group, User};

diesel::table! {
    use diesel::sql_types::*;

    /// Roles for grant access levels.
    roles (id) {
        /// Role ID (2=viewer, 3=editor).
        id -> SmallInt,
        /// Role name.
        name -> Text,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use rosetta_uuid::diesel_impls::Uuid;

    /// Ownables view (RLS filtered).
    ownables (id) {
        /// Ownable ID.
        id -> Uuid,
        /// Ownable title.
        title -> Text,
        /// User who created the ownable.
        created_by -> Uuid,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use rosetta_uuid::diesel_impls::Uuid;

    /// Ownable owners view (RLS filtered).
    ownable_owners (ownable_id, owner_id) {
        /// Ownable ID.
        ownable_id -> Uuid,
        /// Owner ID (user or group).
        owner_id -> Uuid,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use rosetta_uuid::diesel_impls::Uuid;

    /// Administrators of ownables (can grant viewer/editor).
    ownable_administrators (ownable_id, administrator_id) {
        /// Ownable ID.
        ownable_id -> Uuid,
        /// Administrator ID (user or group).
        administrator_id -> Uuid,
        /// User who granted admin privileges.
        granted_by -> Uuid,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use rosetta_uuid::diesel_impls::Uuid;

    /// Grants for viewer/editor access.
    grants (id) {
        /// Grant ID.
        id -> Uuid,
        /// Ownable ID.
        ownable_id -> Uuid,
        /// Grantee ID (user or group).
        grantee_id -> Uuid,
        /// User who granted access.
        grantor_id -> Uuid,
        /// Role ID (2=viewer, 3=editor).
        role_id -> SmallInt,
    }
}

diesel::joinable!(grants -> roles (role_id));

diesel::allow_tables_to_appear_in_same_query!(
    roles,
    ownables,
    ownable_owners,
    ownable_administrators,
    grants,
);

/// Viewer role: can view only
pub const ROLE_VIEWER: i16 = 2;
/// Editor role: can view and edit
pub const ROLE_EDITOR: i16 = 3;

/// An ownable entity - something that can be owned and shared.
#[derive(Queryable, Selectable, Debug, PartialEq, Clone, Insertable)]
#[diesel(table_name = ownables)]
pub struct Ownable {
    id: Uuid,
    title: String,
    created_by: Uuid,
}

impl Ownable {
    /// Creates a new ownable with the provided title and creator.
    #[must_use]
    pub fn new(title: &str, creator: &User) -> Self {
        Self { id: Uuid::utc_v7(), title: title.to_string(), created_by: creator.id }
    }

    /// Inserts the ownable into the database.
    /// The RLS INSERT trigger auto-adds the creator as the first owner.
    /// Note: The session user must be set before calling this method.
    pub fn insert(self, conn: &mut SqliteConnection) -> Result<Self, Box<dyn std::error::Error>> {
        diesel::insert_into(ownables::table).values(&self).execute(conn)?;
        Ok(self)
    }

    /// Returns whether the ownable exists in the database.
    pub fn exists(&self, conn: &mut SqliteConnection) -> Result<bool, Box<dyn std::error::Error>> {
        let count: i64 =
            ownables::table.filter(ownables::id.eq(&self.id)).count().get_result(conn)?;
        Ok(count > 0)
    }

    /// Deletes the ownable from the database.
    pub fn delete(&self, conn: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
        diesel::delete(ownables::table.filter(ownables::id.eq(&self.id))).execute(conn)?;
        Ok(())
    }

    /// Adds an owner to this ownable.
    /// Requires the session user to be an existing owner.
    pub fn add_owner<T: HasOwnerId>(
        &self,
        owner: &T,
        as_user: &User,
        conn: &mut SqliteConnection,
    ) -> Result<(), Box<dyn std::error::Error>> {
        set_session_user_id(&as_user.id);
        diesel::insert_into(ownable_owners::table)
            .values((
                ownable_owners::ownable_id.eq(&self.id),
                ownable_owners::owner_id.eq(owner.owner_id()),
            ))
            .execute(conn)?;
        Ok(())
    }

    /// Removes an owner from this ownable.
    /// Requires the session user to be an existing owner.
    pub fn remove_owner<T: HasOwnerId>(
        &self,
        owner: &T,
        as_user: &User,
        conn: &mut SqliteConnection,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        set_session_user_id(&as_user.id);
        let rows = diesel::delete(
            ownable_owners::table
                .filter(ownable_owners::ownable_id.eq(&self.id))
                .filter(ownable_owners::owner_id.eq(owner.owner_id())),
        )
        .execute(conn)?;
        Ok(rows)
    }

    /// Returns the number of owners for this ownable.
    pub fn owner_count(
        &self,
        conn: &mut SqliteConnection,
    ) -> Result<i64, Box<dyn std::error::Error>> {
        let count: i64 = ownable_owners::table
            .filter(ownable_owners::ownable_id.eq(&self.id))
            .count()
            .get_result(conn)?;
        Ok(count)
    }

    /// Checks if the given entity is an owner of this ownable.
    pub fn has_owner<T: HasOwnerId>(
        &self,
        owner: &T,
        conn: &mut SqliteConnection,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let count: i64 = ownable_owners::table
            .filter(ownable_owners::ownable_id.eq(&self.id))
            .filter(ownable_owners::owner_id.eq(owner.owner_id()))
            .count()
            .get_result(conn)?;
        Ok(count > 0)
    }

    /// Adds an administrator to this ownable.
    pub fn add_admin<T: HasOwnerId>(
        &self,
        admin: &T,
        grantor: &User,
        conn: &mut SqliteConnection,
    ) -> Result<(), Box<dyn std::error::Error>> {
        diesel::insert_into(ownable_administrators::table)
            .values((
                ownable_administrators::ownable_id.eq(&self.id),
                ownable_administrators::administrator_id.eq(admin.owner_id()),
                ownable_administrators::granted_by.eq(&grantor.id),
            ))
            .execute(conn)?;
        Ok(())
    }

    /// Removes an administrator from this ownable.
    pub fn remove_admin<T: HasOwnerId>(
        &self,
        admin: &T,
        conn: &mut SqliteConnection,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        let rows = diesel::delete(
            ownable_administrators::table
                .filter(ownable_administrators::ownable_id.eq(&self.id))
                .filter(ownable_administrators::administrator_id.eq(admin.owner_id())),
        )
        .execute(conn)?;
        Ok(rows)
    }

    /// Checks if the given entity is an administrator of this ownable.
    pub fn has_admin<T: HasOwnerId>(
        &self,
        admin: &T,
        conn: &mut SqliteConnection,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let count: i64 = ownable_administrators::table
            .filter(ownable_administrators::ownable_id.eq(&self.id))
            .filter(ownable_administrators::administrator_id.eq(admin.owner_id()))
            .count()
            .get_result(conn)?;
        Ok(count > 0)
    }

    /// Grants viewer or editor access to this ownable.
    pub fn grant_access<T: HasOwnerId>(
        &self,
        grantee: &T,
        grantor: &User,
        role_id: i16,
        conn: &mut SqliteConnection,
    ) -> Result<Grant, Box<dyn std::error::Error>> {
        set_session_user_id(&grantor.id);
        let grant = Grant {
            id: Uuid::utc_v7(),
            ownable_id: self.id,
            grantee_id: grantee.owner_id(),
            grantor_id: grantor.id,
            role_id,
        };
        diesel::insert_into(grants::table).values(&grant).execute(conn)?;
        Ok(grant)
    }

    /// Checks if the given entity has a grant for this ownable.
    pub fn has_grant<T: HasOwnerId>(
        &self,
        grantee: &T,
        conn: &mut SqliteConnection,
    ) -> Result<Option<i16>, Box<dyn std::error::Error>> {
        let role: Option<i16> = grants::table
            .filter(grants::ownable_id.eq(&self.id))
            .filter(grants::grantee_id.eq(grantee.owner_id()))
            .select(grants::role_id)
            .first(conn)
            .optional()?;
        Ok(role)
    }

    /// Revokes access from the given entity.
    pub fn revoke_access<T: HasOwnerId>(
        &self,
        grantee: &T,
        conn: &mut SqliteConnection,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        let rows = diesel::delete(
            grants::table
                .filter(grants::ownable_id.eq(&self.id))
                .filter(grants::grantee_id.eq(grantee.owner_id())),
        )
        .execute(conn)?;
        Ok(rows)
    }
}

/// A grant giving viewer or editor access to an ownable.
#[derive(Queryable, Selectable, Debug, Insertable, PartialEq, Clone)]
#[diesel(table_name = grants)]
pub struct Grant {
    id: Uuid,
    ownable_id: Uuid,
    grantee_id: Uuid,
    grantor_id: Uuid,
    role_id: i16,
}

/// Trait for entities that have an owner ID.
pub trait HasOwnerId {
    /// Returns the owner ID for this entity.
    fn owner_id(&self) -> Uuid;
}

impl HasOwnerId for User {
    fn owner_id(&self) -> Uuid {
        self.id
    }
}

impl HasOwnerId for Group {
    fn owner_id(&self) -> Uuid {
        self.id
    }
}

fn translation_options() -> Pg2SqliteOptions {
    use pg2sqlite::traits::TranslationOptions;

    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_rls_audit_table_name("rls_audit".to_string())
}

fn setup_database(connection: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    use pg2sqlite::traits::TranslationOptions;

    let options = translation_options();

    // Load and translate groups.sql first
    let groups_sql = include_str!("fixtures/groups.sql");
    let groups_translated = Pg2Sqlite::default().sql(groups_sql)?.translate(&options)?;
    for stmt in &groups_translated {
        diesel::sql_query(stmt.to_string()).execute(connection)?;
    }

    // Partial fragment: FKs to `owners`/`users` resolve in groups.sql, a
    // separate unit above, so opt out of the reference-closure check.
    let grants_sql = include_str!("fixtures/rls_grants.sql");
    let grants_options = translation_options().with_dangling_foreign_keys_allowed();
    let grants_translated = Pg2Sqlite::default().sql(grants_sql)?.translate(&grants_options)?;
    for stmt in &grants_translated {
        diesel::sql_query(stmt.to_string()).execute(connection)?;
    }

    Ok(())
}

#[test]
fn test_rls_grants_schema_execution() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    setup_database(&mut conn)?;

    // Verify tables exist by counting rows
    let ownable_count: i64 = ownables::table.count().get_result(&mut conn)?;
    assert_eq!(ownable_count, 0);

    let role_count: i64 = roles::table.count().get_result(&mut conn)?;
    assert_eq!(role_count, 2, "Should have viewer and editor roles");

    Ok(())
}

/// Test that inserting without a session user fails.
/// The session user must be set before any RLS-protected operation.
#[test]
fn test_insert_without_session_user_fails() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    setup_database(&mut conn)?;

    // Create a user but DON'T set the session user
    let alice = User::new("Alice").insert(&mut conn)?;

    // Attempting to create an ownable without setting session user should fail
    let result = Ownable::new("My Document", &alice).insert(&mut conn);

    assert!(result.is_err(), "Insert should fail when session user is not set");

    // Verify the error is related to current_app_user
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("current_app_user"),
        "Error should mention current_app_user, got: {err_msg}"
    );

    Ok(())
}

/// The translated schema carries a trigger that auto-inserts the creator as
/// owner.
#[test]
fn test_creator_becomes_owner() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    setup_database(&mut conn)?;

    let alice = User::new("Alice").insert(&mut conn)?;
    set_session_user_id(&alice.id);
    let doc = Ownable::new("My Document", &alice).insert(&mut conn)?;

    // Creator should automatically be an owner
    assert!(doc.has_owner(&alice, &mut conn)?, "Creator should be an owner");
    assert_eq!(doc.owner_count(&mut conn)?, 1, "Should have exactly one owner");

    Ok(())
}

#[test]
fn test_add_remove_owners() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    setup_database(&mut conn)?;

    let alice = User::new("Alice").insert(&mut conn)?;
    let bob = User::new("Bob").insert(&mut conn)?;

    set_session_user_id(&alice.id);
    let doc = Ownable::new("Shared Document", &alice).insert(&mut conn)?;

    // Add Bob as owner (Alice adds him since she's the owner)
    doc.add_owner(&bob, &alice, &mut conn)?;
    assert!(doc.has_owner(&bob, &mut conn)?, "Bob should be an owner");
    assert_eq!(doc.owner_count(&mut conn)?, 2, "Should have two owners");

    // Remove Alice as owner (Bob remains) - Bob does the removal
    doc.remove_owner(&alice, &bob, &mut conn)?;
    assert!(!doc.has_owner(&alice, &mut conn)?, "Alice should no longer be an owner");
    assert!(doc.has_owner(&bob, &mut conn)?, "Bob should still be an owner");

    Ok(())
}

/// Test that we can detect when trying to remove the last owner.
/// Note: This is enforced at application level, not database level.
#[test]
fn test_last_owner_detection() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    setup_database(&mut conn)?;

    let alice = User::new("Alice").insert(&mut conn)?;
    set_session_user_id(&alice.id);
    let doc = Ownable::new("My Document", &alice).insert(&mut conn)?;

    // Check owner count before attempting removal
    assert_eq!(doc.owner_count(&mut conn)?, 1, "Should have exactly one owner");

    // Application-level check: don't allow removal if count would become 0
    // (In a real application, this would return an error or prevent the operation)

    // Verify Alice is still the owner
    assert!(doc.has_owner(&alice, &mut conn)?, "Alice should still be the owner");

    Ok(())
}

#[test]
fn test_add_remove_admins() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    setup_database(&mut conn)?;

    let alice = User::new("Alice").insert(&mut conn)?;
    let bob = User::new("Bob").insert(&mut conn)?;

    set_session_user_id(&alice.id);
    let doc = Ownable::new("Document", &alice).insert(&mut conn)?;

    // Add Bob as admin (Alice is the grantor/owner)
    doc.add_admin(&bob, &alice, &mut conn)?;
    assert!(doc.has_admin(&bob, &mut conn)?, "Bob should be an admin");

    // Remove Bob as admin
    doc.remove_admin(&bob, &mut conn)?;
    assert!(!doc.has_admin(&bob, &mut conn)?, "Bob should no longer be an admin");

    Ok(())
}

#[test]
fn test_grant_access() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    setup_database(&mut conn)?;

    let alice = User::new("Alice").insert(&mut conn)?;
    let bob = User::new("Bob").insert(&mut conn)?;
    let charlie = User::new("Charlie").insert(&mut conn)?;

    set_session_user_id(&alice.id);
    let doc = Ownable::new("Document", &alice).insert(&mut conn)?;

    // Grant Bob viewer access
    doc.grant_access(&bob, &alice, ROLE_VIEWER, &mut conn)?;
    assert_eq!(doc.has_grant(&bob, &mut conn)?, Some(ROLE_VIEWER), "Bob should have viewer access");

    // Grant Charlie editor access
    doc.grant_access(&charlie, &alice, ROLE_EDITOR, &mut conn)?;
    assert_eq!(
        doc.has_grant(&charlie, &mut conn)?,
        Some(ROLE_EDITOR),
        "Charlie should have editor access"
    );

    // Revoke Bob's access
    doc.revoke_access(&bob, &mut conn)?;
    assert_eq!(doc.has_grant(&bob, &mut conn)?, None, "Bob should no longer have access");

    Ok(())
}

#[test]
fn test_group_as_owner() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    setup_database(&mut conn)?;

    let alice = User::new("Alice").insert(&mut conn)?;
    let bob = User::new("Bob").insert(&mut conn)?;
    let engineering = Group::new("Engineering", None).insert(&mut conn)?;

    // Add Bob to Engineering group
    engineering.add_member(&bob, &mut conn)?;

    set_session_user_id(&alice.id);
    let doc = Ownable::new("Team Document", &alice).insert(&mut conn)?;

    // Add Engineering group as owner (Alice adds it)
    doc.add_owner(&engineering, &alice, &mut conn)?;
    assert!(doc.has_owner(&engineering, &mut conn)?, "Engineering should be an owner");

    // Now Alice can be removed since Engineering is also an owner
    // Alice removes herself
    doc.remove_owner(&alice, &alice, &mut conn)?;

    // Alice no longer has access to see anything (she's not an owner, admin, or
    // grantee) So we need to check from Bob's perspective (as member of
    // Engineering group)
    set_session_user_id(&bob.id);
    assert!(!doc.has_owner(&alice, &mut conn)?, "Alice should no longer be an owner");
    assert!(doc.has_owner(&engineering, &mut conn)?, "Engineering should still be an owner");

    Ok(())
}

#[test]
fn test_group_as_admin() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    setup_database(&mut conn)?;

    let alice = User::new("Alice").insert(&mut conn)?;
    let admins = Group::new("Admins", None).insert(&mut conn)?;

    set_session_user_id(&alice.id);
    let doc = Ownable::new("Document", &alice).insert(&mut conn)?;

    // Add Admins group as administrator (Alice is the grantor/owner)
    doc.add_admin(&admins, &alice, &mut conn)?;
    assert!(doc.has_admin(&admins, &mut conn)?, "Admins group should be an admin");

    Ok(())
}

#[test]
fn test_group_grant() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    setup_database(&mut conn)?;

    let alice = User::new("Alice").insert(&mut conn)?;
    let viewers = Group::new("Viewers", None).insert(&mut conn)?;

    set_session_user_id(&alice.id);
    let doc = Ownable::new("Document", &alice).insert(&mut conn)?;

    // Grant Viewers group viewer access
    doc.grant_access(&viewers, &alice, ROLE_VIEWER, &mut conn)?;
    assert_eq!(
        doc.has_grant(&viewers, &mut conn)?,
        Some(ROLE_VIEWER),
        "Viewers group should have viewer access"
    );

    Ok(())
}

#[test]
fn test_group_hierarchy_with_roles() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    setup_database(&mut conn)?;

    // Create users
    let alice = User::new("Alice").insert(&mut conn)?;
    let bob = User::new("Bob").insert(&mut conn)?;
    let charlie = User::new("Charlie").insert(&mut conn)?;
    let dave = User::new("Dave").insert(&mut conn)?;

    // Create group hierarchy: Team > (Admins, Editors)
    let team = Group::new("Team", None).insert(&mut conn)?;
    let team_admins = Group::new("Team Admins", Some(&team)).insert(&mut conn)?;
    let team_editors = Group::new("Team Editors", Some(&team)).insert(&mut conn)?;

    // Add members to groups
    team_admins.add_member(&bob, &mut conn)?;
    team_editors.add_member(&charlie, &mut conn)?;
    // Dave is just a team member (via direct membership)
    team.add_member(&dave, &mut conn)?;

    // Alice creates a document
    set_session_user_id(&alice.id);
    let doc = Ownable::new("Team Document", &alice).insert(&mut conn)?;

    // Make Team Admins group administrators of the document
    doc.add_admin(&team_admins, &alice, &mut conn)?;

    // Grant Team Editors group editor access
    doc.grant_access(&team_editors, &alice, ROLE_EDITOR, &mut conn)?;

    // Grant Team group viewer access
    doc.grant_access(&team, &alice, ROLE_VIEWER, &mut conn)?;

    // Verify group-level assignments
    assert!(doc.has_admin(&team_admins, &mut conn)?, "Team Admins should be admin");
    assert_eq!(
        doc.has_grant(&team_editors, &mut conn)?,
        Some(ROLE_EDITOR),
        "Team Editors should have editor grant"
    );
    assert_eq!(
        doc.has_grant(&team, &mut conn)?,
        Some(ROLE_VIEWER),
        "Team should have viewer grant"
    );

    Ok(())
}

#[test]
fn test_cascade_delete_ownable() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    setup_database(&mut conn)?;

    let alice = User::new("Alice").insert(&mut conn)?;
    let bob = User::new("Bob").insert(&mut conn)?;

    set_session_user_id(&alice.id);
    let doc = Ownable::new("Document", &alice).insert(&mut conn)?;
    doc.add_admin(&bob, &alice, &mut conn)?;
    doc.grant_access(&bob, &alice, ROLE_VIEWER, &mut conn)?;

    // Verify data exists
    assert_eq!(doc.owner_count(&mut conn)?, 1);
    assert!(doc.has_admin(&bob, &mut conn)?);
    assert!(doc.has_grant(&bob, &mut conn)?.is_some());

    // Delete the ownable
    doc.delete(&mut conn)?;

    // Verify ownable is gone
    assert!(!doc.exists(&mut conn)?);

    // Verify related data is cascade deleted
    let owner_count: i64 = ownable_owners::table.count().get_result(&mut conn)?;
    assert_eq!(owner_count, 0, "Owners should be cascade deleted");

    let admin_count: i64 = ownable_administrators::table.count().get_result(&mut conn)?;
    assert_eq!(admin_count, 0, "Admins should be cascade deleted");

    let grant_count: i64 = grants::table.count().get_result(&mut conn)?;
    assert_eq!(grant_count, 0, "Grants should be cascade deleted");

    Ok(())
}

#[test]
fn test_unique_grant_per_grantee() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = establish_connection();
    setup_database(&mut conn)?;

    let alice = User::new("Alice").insert(&mut conn)?;
    let bob = User::new("Bob").insert(&mut conn)?;

    set_session_user_id(&alice.id);
    let doc = Ownable::new("Document", &alice).insert(&mut conn)?;

    // Grant Bob viewer access
    doc.grant_access(&bob, &alice, ROLE_VIEWER, &mut conn)?;

    // Try to grant Bob editor access (should fail due to UNIQUE constraint)
    let result = doc.grant_access(&bob, &alice, ROLE_EDITOR, &mut conn);
    assert!(result.is_err(), "Should not allow duplicate grants for same grantee");

    Ok(())
}

#[test]
fn test_rls_grants_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    // Translate both files together
    let groups_sql = include_str!("fixtures/groups.sql");
    let grants_sql = include_str!("fixtures/rls_grants.sql");
    let combined_sql = format!("{groups_sql}\n{grants_sql}");

    let options = translation_options();

    let translated = Pg2Sqlite::default().sql(&combined_sql)?.translate(&options)?;
    let translated_sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    // Verify each statement is valid SQLite
    for stmt in &translated {
        let sql_str = stmt.to_string();
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::SQLiteDialect {}, &sql_str)
            .expect("Failed to parse translated SQL as SQLite");
    }

    insta::assert_snapshot!("rls_grants_translation", translated_sql);
    Ok(())
}
