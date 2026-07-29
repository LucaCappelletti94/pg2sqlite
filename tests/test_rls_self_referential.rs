//! Test for RLS policies on self-referential user tables.
//!
//! Scenario: A users table where each user can only see and edit their
//! own record. This is a common pattern for user profile tables.

mod helpers;

use diesel::prelude::*;
use helpers::{establish_connection, set_session_user_id};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation},
    traits::TranslationOptions,
};
use rosetta_uuid::Uuid;

mod schema {
    diesel::table! {
        /// App users view with RLS filtering based on current user.
        app_users (id) {
            id -> Binary,
            email -> Text,
            display_name -> Nullable<Text>,
            settings -> Nullable<Text>,
        }
    }
}

use schema::app_users;

#[derive(Queryable, Insertable, Selectable, Debug, Clone)]
#[diesel(table_name = app_users)]
struct AppUser {
    id: Vec<u8>,
    email: String,
    display_name: Option<String>,
    settings: Option<String>,
}

fn translate_and_setup() -> Result<diesel::SqliteConnection, Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_self_referential.sql");

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string())
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

/// Snapshot test for self-referential RLS translation.
#[test]
fn test_self_referential_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_self_referential.sql");

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string())
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("rls_self_referential_translation", translated_sql);

    Ok(())
}

#[test]
fn test_user_sees_only_self() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice_id = Uuid::new_v4();
    let bob_id = Uuid::new_v4();

    // Create Alice's user record
    set_session_user_id(&alice_id);
    diesel::insert_into(app_users::table)
        .values(AppUser {
            id: alice_id.as_bytes().to_vec(),
            email: "alice@example.com".to_string(),
            display_name: Some("Alice".to_string()),
            settings: None,
        })
        .execute(&mut connection)?;

    // Create Bob's user record
    set_session_user_id(&bob_id);
    diesel::insert_into(app_users::table)
        .values(AppUser {
            id: bob_id.as_bytes().to_vec(),
            email: "bob@example.com".to_string(),
            display_name: Some("Bob".to_string()),
            settings: None,
        })
        .execute(&mut connection)?;

    // Alice should only see her own record
    set_session_user_id(&alice_id);
    let alice_visible = app_users::table.select(AppUser::as_select()).load(&mut connection)?;
    assert_eq!(alice_visible.len(), 1, "Alice should see 1 user");
    assert_eq!(alice_visible[0].email, "alice@example.com");

    // Bob should only see his own record
    set_session_user_id(&bob_id);
    let bob_visible = app_users::table.select(AppUser::as_select()).load(&mut connection)?;
    assert_eq!(bob_visible.len(), 1, "Bob should see 1 user");
    assert_eq!(bob_visible[0].email, "bob@example.com");

    Ok(())
}

#[test]
fn test_user_cannot_see_others() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice_id = Uuid::new_v4();
    let bob_id = Uuid::new_v4();
    let charlie_id = Uuid::new_v4();

    // Create records for Alice and Bob
    set_session_user_id(&alice_id);
    diesel::insert_into(app_users::table)
        .values(AppUser {
            id: alice_id.as_bytes().to_vec(),
            email: "alice@example.com".to_string(),
            display_name: Some("Alice".to_string()),
            settings: None,
        })
        .execute(&mut connection)?;

    set_session_user_id(&bob_id);
    diesel::insert_into(app_users::table)
        .values(AppUser {
            id: bob_id.as_bytes().to_vec(),
            email: "bob@example.com".to_string(),
            display_name: Some("Bob".to_string()),
            settings: None,
        })
        .execute(&mut connection)?;

    // Charlie (who has no record) should see 0 users
    set_session_user_id(&charlie_id);
    let charlie_visible = app_users::table.select(AppUser::as_select()).load(&mut connection)?;
    assert_eq!(charlie_visible.len(), 0, "Charlie should see 0 users");

    Ok(())
}

#[test]
fn test_user_can_update_own() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice_id = Uuid::new_v4();

    set_session_user_id(&alice_id);
    diesel::insert_into(app_users::table)
        .values(AppUser {
            id: alice_id.as_bytes().to_vec(),
            email: "alice@example.com".to_string(),
            display_name: Some("Alice".to_string()),
            settings: None,
        })
        .execute(&mut connection)?;

    // Alice updates her display name
    diesel::update(app_users::table.filter(app_users::id.eq(alice_id.as_bytes().to_vec())))
        .set(app_users::display_name.eq("Alice Updated"))
        .execute(&mut connection)?;

    let updated = app_users::table
        .filter(app_users::id.eq(alice_id.as_bytes().to_vec()))
        .select(AppUser::as_select())
        .first(&mut connection)?;
    assert_eq!(updated.display_name, Some("Alice Updated".to_string()));

    Ok(())
}

#[test]
fn test_user_cannot_update_others() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice_id = Uuid::new_v4();
    let bob_id = Uuid::new_v4();

    set_session_user_id(&alice_id);
    diesel::insert_into(app_users::table)
        .values(AppUser {
            id: alice_id.as_bytes().to_vec(),
            email: "alice@example.com".to_string(),
            display_name: Some("Alice".to_string()),
            settings: None,
        })
        .execute(&mut connection)?;

    set_session_user_id(&bob_id);
    diesel::insert_into(app_users::table)
        .values(AppUser {
            id: bob_id.as_bytes().to_vec(),
            email: "bob@example.com".to_string(),
            display_name: Some("Bob".to_string()),
            settings: None,
        })
        .execute(&mut connection)?;

    // Bob tries to update Alice's record (should affect 0 rows)
    diesel::update(app_users::table.filter(app_users::id.eq(alice_id.as_bytes().to_vec())))
        .set(app_users::display_name.eq("Hacked"))
        .execute(&mut connection)?;

    // Verify Alice's record was NOT changed
    set_session_user_id(&alice_id);
    let alice_record = app_users::table
        .filter(app_users::id.eq(alice_id.as_bytes().to_vec()))
        .select(AppUser::as_select())
        .first(&mut connection)?;
    assert_eq!(
        alice_record.display_name,
        Some("Alice".to_string()),
        "Alice's name should NOT be changed by Bob"
    );

    Ok(())
}

/// Test delete behavior when no explicit DELETE policy exists. Per the
/// deny-by-default semantics PostgreSQL applies once RLS is enabled, a DELETE
/// against a table that has no `FOR DELETE` policy must be rejected -
/// regardless of whether the user could `SELECT` the row.
#[test]
fn test_delete_without_explicit_policy() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice_id = Uuid::new_v4();

    // Seed a row Alice owns by going through the INSERT policy.
    set_session_user_id(&alice_id);
    diesel::insert_into(app_users::table)
        .values(AppUser {
            id: alice_id.as_bytes().to_vec(),
            email: "alice@example.com".to_string(),
            display_name: Some("Alice".to_string()),
            settings: None,
        })
        .execute(&mut connection)?;

    // Even the row owner cannot DELETE through the view when no FOR DELETE
    // policy is declared. pg2sqlite emits a RAISE(ABORT) inside the INSTEAD
    // OF DELETE trigger that mirrors PostgreSQL's "permission denied" deny.
    let result =
        diesel::delete(app_users::table.filter(app_users::id.eq(alice_id.as_bytes().to_vec())))
            .execute(&mut connection);
    assert!(
        result.is_err(),
        "DELETE must be rejected when no FOR DELETE policy is declared, got Ok({result:?})"
    );

    // Row must still exist.
    let count = app_users::table.count().get_result::<i64>(&mut connection)?;
    assert_eq!(count, 1, "row must survive the rejected DELETE");

    Ok(())
}

#[test]
fn test_insert_wrong_id_fails() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice_id = Uuid::new_v4();
    let bob_id = Uuid::new_v4();

    // Alice tries to insert a record with Bob's ID (should fail)
    set_session_user_id(&alice_id);
    let result = diesel::insert_into(app_users::table)
        .values(AppUser {
            id: bob_id.as_bytes().to_vec(), // Wrong ID!
            email: "fake@example.com".to_string(),
            display_name: None,
            settings: None,
        })
        .execute(&mut connection);

    assert!(result.is_err(), "Insert with wrong ID should fail");

    Ok(())
}
