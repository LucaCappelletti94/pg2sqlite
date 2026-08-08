//! Test for current_user based RLS policies.
//!
//! Scenario: RLS policies that use PostgreSQL's `current_user` instead of
//! `current_setting()`. The translation should map `current_user` to a
//! configured SQLite function.

mod helpers;

use diesel::prelude::*;
use helpers::{establish_connection, set_session_username};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};
use rosetta_uuid::Uuid;

mod schema {
    diesel::table! {
        /// Profiles view with RLS filtering based on current_user (username).
        profiles (id) {
            id -> Binary,
            username -> Text,
            email -> Text,
            is_public -> Bool,
        }
    }
}

use schema::profiles;

#[derive(Queryable, Insertable, Selectable, Debug, Clone)]
#[diesel(table_name = profiles)]
struct Profile {
    id: Vec<u8>,
    username: String,
    email: String,
    is_public: bool,
}

fn translate_and_setup() -> Result<diesel::SqliteConnection, Box<dyn std::error::Error>> {
    use pg2sqlite::traits::TranslationOptions;

    let sql = include_str!("fixtures/rls_current_user.sql");

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_user("current_app_username"));

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection = establish_connection();

    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    Ok(connection)
}

/// Snapshot test for current_user RLS translation.
#[test]
fn test_current_user_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    use pg2sqlite::traits::TranslationOptions;

    let sql = include_str!("fixtures/rls_current_user.sql");

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_user("current_app_username"));

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("rls_current_user_translation", translated_sql);

    Ok(())
}

#[test]
fn test_user_sees_own_profile() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    set_session_username("alice");
    diesel::insert_into(profiles::table)
        .values(Profile {
            id: Uuid::new_v4().as_bytes().to_vec(),
            username: "alice".to_string(),
            email: "alice@example.com".to_string(),
            is_public: false,
        })
        .execute(&mut connection)?;

    let alice_profiles = profiles::table.select(Profile::as_select()).load(&mut connection)?;
    assert_eq!(alice_profiles.len(), 1, "Alice should see 1 profile");
    assert_eq!(alice_profiles[0].username, "alice");

    Ok(())
}

#[test]
fn test_user_sees_public_profiles() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // Create a public profile for Bob
    set_session_username("bob");
    diesel::insert_into(profiles::table)
        .values(Profile {
            id: Uuid::new_v4().as_bytes().to_vec(),
            username: "bob".to_string(),
            email: "bob@example.com".to_string(),
            is_public: true,
        })
        .execute(&mut connection)?;

    // Create a private profile for Charlie
    set_session_username("charlie");
    diesel::insert_into(profiles::table)
        .values(Profile {
            id: Uuid::new_v4().as_bytes().to_vec(),
            username: "charlie".to_string(),
            email: "charlie@example.com".to_string(),
            is_public: false,
        })
        .execute(&mut connection)?;

    // Alice should see Bob's public profile but not Charlie's private one
    set_session_username("alice");
    let visible_profiles = profiles::table.select(Profile::as_select()).load(&mut connection)?;
    assert_eq!(visible_profiles.len(), 1, "Alice should see 1 profile (Bob's public one)");
    assert_eq!(visible_profiles[0].username, "bob");

    Ok(())
}

#[test]
fn test_user_cannot_see_private_others() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    // Create a private profile for Bob
    set_session_username("bob");
    diesel::insert_into(profiles::table)
        .values(Profile {
            id: Uuid::new_v4().as_bytes().to_vec(),
            username: "bob".to_string(),
            email: "bob@example.com".to_string(),
            is_public: false,
        })
        .execute(&mut connection)?;

    // Alice should NOT see Bob's private profile
    set_session_username("alice");
    let visible_count = profiles::table.count().get_result::<i64>(&mut connection)?;
    assert_eq!(visible_count, 0, "Alice should see 0 profiles (Bob's is private)");

    Ok(())
}

#[test]
fn test_user_can_update_own_only() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice_id = Uuid::new_v4();
    let bob_id = Uuid::new_v4();

    set_session_username("alice");
    diesel::insert_into(profiles::table)
        .values(Profile {
            id: alice_id.as_bytes().to_vec(),
            username: "alice".to_string(),
            email: "alice@example.com".to_string(),
            is_public: false,
        })
        .execute(&mut connection)?;

    set_session_username("bob");
    diesel::insert_into(profiles::table)
        .values(Profile {
            id: bob_id.as_bytes().to_vec(),
            username: "bob".to_string(),
            email: "bob@example.com".to_string(),
            is_public: true,
        })
        .execute(&mut connection)?;

    // Alice can update her own profile
    set_session_username("alice");
    diesel::update(profiles::table.filter(profiles::id.eq(alice_id.as_bytes().to_vec())))
        .set(profiles::email.eq("alice_updated@example.com"))
        .execute(&mut connection)?;

    let alice_profile = profiles::table
        .filter(profiles::id.eq(alice_id.as_bytes().to_vec()))
        .select(Profile::as_select())
        .first(&mut connection)?;
    assert_eq!(alice_profile.email, "alice_updated@example.com");

    // Alice cannot update Bob's profile (update will affect 0 rows)
    // Bob's profile is visible but the USING clause prevents Alice from updating it
    diesel::update(profiles::table.filter(profiles::id.eq(bob_id.as_bytes().to_vec())))
        .set(profiles::email.eq("hacked@example.com"))
        .execute(&mut connection)?;

    // Verify Bob's email was NOT changed
    set_session_username("bob");
    let bob_profile = profiles::table
        .filter(profiles::id.eq(bob_id.as_bytes().to_vec()))
        .select(Profile::as_select())
        .first(&mut connection)?;
    assert_eq!(bob_profile.email, "bob@example.com", "Bob's email should NOT be changed by Alice");

    Ok(())
}
