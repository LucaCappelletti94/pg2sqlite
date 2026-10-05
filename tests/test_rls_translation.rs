//! Test for RLS (Row-Level Security) translation from PostgreSQL to SQLite.
//!
//! Scenario: A documents table with RLS policies that filter rows based on
//! the current user ID. The translation should produce:
//! 1. A renamed table (documents_rls) containing the actual data
//! 2. A view (documents) that filters rows based on the session user function
//! 3. INSTEAD OF triggers for INSERT, UPDATE, DELETE operations

mod helpers;

use diesel::prelude::*;
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};
use rosetta_uuid::Uuid;

/// Diesel table definition for the documents view.
/// The RLS translation makes this transparent - all operations go through
/// triggers.
mod schema {
    diesel::table! {
        /// Documents table/view with RLS filtering based on current_app_user().
        documents (id) {
            /// Primary key UUID.
            id -> Binary,
            /// Owner's UUID.
            owner_id -> Binary,
            /// Document title.
            title -> Text,
            /// Document content.
            content -> Text,
        }
    }
}

use schema::documents;

use crate::helpers::set_session_user_id;

#[derive(Queryable, Insertable, Selectable, Debug)]
#[diesel(table_name = documents)]
struct Document {
    /// Primary key UUID.
    id: Vec<u8>,
    /// Owner's UUID.
    owner_id: Vec<u8>,
    /// Document title.
    title: String,
    /// Document content.
    content: String,
}

#[test]
fn test_rls_select_policy_filters_rows() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_basic.sql");

    // Configure translation with RLS options
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));

    // Translate the SQL
    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    // Snapshot the translated SQL for consistency checking
    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("rls_basic_translation", translated_sql);

    let mut connection = establish_connection();

    // Run the translations
    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    // Create two users
    let user_alice = Uuid::new_v4();
    let user_bob = Uuid::new_v4();

    // Insert documents through the RLS view (triggers handle the actual insert)
    // As Alice, insert Alice's documents
    set_session_user_id(&user_alice);
    diesel::insert_into(documents::table)
        .values(&[
            Document {
                id: Uuid::new_v4().as_bytes().to_vec(),
                owner_id: user_alice.as_bytes().to_vec(),
                title: "Alice Doc 1".to_string(),
                content: "Alice content 1".to_string(),
            },
            Document {
                id: Uuid::new_v4().as_bytes().to_vec(),
                owner_id: user_alice.as_bytes().to_vec(),
                title: "Alice Doc 2".to_string(),
                content: "Alice content 2".to_string(),
            },
        ])
        .execute(&mut connection)?;

    // As Bob, insert Bob's document
    set_session_user_id(&user_bob);
    diesel::insert_into(documents::table)
        .values(Document {
            id: Uuid::new_v4().as_bytes().to_vec(),
            owner_id: user_bob.as_bytes().to_vec(),
            title: "Bob Doc 1".to_string(),
            content: "Bob content 1".to_string(),
        })
        .execute(&mut connection)?;

    // Test: As Alice, should see only Alice's documents (2 docs)
    set_session_user_id(&user_alice);
    let alice_count = documents::table.count().get_result::<i64>(&mut connection)?;
    assert_eq!(alice_count, 2, "Alice should see 2 documents, but saw {alice_count}");

    // Test: As Bob, should see only Bob's documents (1 doc)
    set_session_user_id(&user_bob);
    let bob_count = documents::table.count().get_result::<i64>(&mut connection)?;
    assert_eq!(bob_count, 1, "Bob should see 1 document, but saw {bob_count}");

    // Test: As a new user with no documents, should see 0 docs
    let user_charlie = Uuid::new_v4();
    set_session_user_id(&user_charlie);
    let charlie_count = documents::table.count().get_result::<i64>(&mut connection)?;
    assert_eq!(charlie_count, 0, "Charlie should see 0 documents, but saw {charlie_count}");

    Ok(())
}

#[test]
fn test_rls_insert_policy_enforces_owner() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_basic.sql");

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

    let user_alice = Uuid::new_v4();
    let user_bob = Uuid::new_v4();

    // Set session as Alice
    set_session_user_id(&user_alice);

    // Test: Alice can insert a document with her own owner_id
    let doc_id = Uuid::new_v4();
    let result = diesel::insert_into(documents::table)
        .values(Document {
            id: doc_id.as_bytes().to_vec(),
            owner_id: user_alice.as_bytes().to_vec(),
            title: "My Doc".to_string(),
            content: "My content".to_string(),
        })
        .execute(&mut connection);
    assert!(result.is_ok(), "Alice should be able to insert a document with her own owner_id");

    // Test: Alice cannot insert a document with Bob's owner_id (policy
    // violation)
    let doc_id2 = Uuid::new_v4();
    let result = diesel::insert_into(documents::table)
        .values(Document {
            id: doc_id2.as_bytes().to_vec(),
            owner_id: user_bob.as_bytes().to_vec(),
            title: "Fake Doc".to_string(),
            content: "Fake content".to_string(),
        })
        .execute(&mut connection);
    assert!(result.is_err(), "Alice should NOT be able to insert a document with Bob's owner_id");

    Ok(())
}

#[test]
fn test_rls_update_policy_restricts_updates() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_basic.sql");

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

    let user_alice = Uuid::new_v4();
    let user_bob = Uuid::new_v4();

    // Insert documents through the RLS view as each user
    let alice_doc_id = Uuid::new_v4();
    set_session_user_id(&user_alice);
    diesel::insert_into(documents::table)
        .values(Document {
            id: alice_doc_id.as_bytes().to_vec(),
            owner_id: user_alice.as_bytes().to_vec(),
            title: "Alice Doc".to_string(),
            content: "Original".to_string(),
        })
        .execute(&mut connection)?;

    let bob_doc_id = Uuid::new_v4();
    set_session_user_id(&user_bob);
    diesel::insert_into(documents::table)
        .values(Document {
            id: bob_doc_id.as_bytes().to_vec(),
            owner_id: user_bob.as_bytes().to_vec(),
            title: "Bob Doc".to_string(),
            content: "Original".to_string(),
        })
        .execute(&mut connection)?;

    // Set session as Alice for update tests
    set_session_user_id(&user_alice);

    // Test: Alice can update her own document
    diesel::update(documents::table.filter(documents::id.eq(alice_doc_id.as_bytes().to_vec())))
        .set(documents::title.eq("Updated Title"))
        .execute(&mut connection)?;

    // Verify the update through the view (Alice can see her own doc)
    let alice_doc = documents::table
        .filter(documents::id.eq(alice_doc_id.as_bytes().to_vec()))
        .select(Document::as_select())
        .first(&mut connection)?;

    assert_eq!(
        alice_doc.title, "Updated Title",
        "Alice's document should be updated to 'Updated Title'"
    );

    // Test: Alice cannot update Bob's document (should not change data due to
    // USING filter)
    diesel::update(documents::table.filter(documents::id.eq(bob_doc_id.as_bytes().to_vec())))
        .set(documents::title.eq("Hacked!"))
        .execute(&mut connection)?;

    // Verify Bob's document was NOT changed (switch to Bob to see his doc)
    set_session_user_id(&user_bob);
    let bob_doc = documents::table
        .filter(documents::id.eq(bob_doc_id.as_bytes().to_vec()))
        .select(Document::as_select())
        .first(&mut connection)?;
    assert_eq!(
        bob_doc.title, "Bob Doc",
        "Alice should NOT be able to update Bob's document - title should remain 'Bob Doc'"
    );

    Ok(())
}

#[test]
fn test_rls_delete_policy_restricts_deletes() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_basic.sql");

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

    let user_alice = Uuid::new_v4();
    let user_bob = Uuid::new_v4();

    // Insert documents through the RLS view as each user
    let alice_doc_id = Uuid::new_v4();
    set_session_user_id(&user_alice);
    diesel::insert_into(documents::table)
        .values(Document {
            id: alice_doc_id.as_bytes().to_vec(),
            owner_id: user_alice.as_bytes().to_vec(),
            title: "Alice Doc".to_string(),
            content: "Content".to_string(),
        })
        .execute(&mut connection)?;

    let bob_doc_id = Uuid::new_v4();
    set_session_user_id(&user_bob);
    diesel::insert_into(documents::table)
        .values(Document {
            id: bob_doc_id.as_bytes().to_vec(),
            owner_id: user_bob.as_bytes().to_vec(),
            title: "Bob Doc".to_string(),
            content: "Content".to_string(),
        })
        .execute(&mut connection)?;

    // Set session as Alice for delete tests
    set_session_user_id(&user_alice);

    // Test: Alice cannot delete Bob's document (should not actually delete due
    // to USING filter)
    diesel::delete(documents::table.filter(documents::id.eq(bob_doc_id.as_bytes().to_vec())))
        .execute(&mut connection)?;

    // Verify Bob's document still exists (switch to Bob to count his docs)
    set_session_user_id(&user_bob);
    let bob_count = documents::table
        .filter(documents::id.eq(bob_doc_id.as_bytes().to_vec()))
        .count()
        .get_result::<i64>(&mut connection)?;
    assert_eq!(
        bob_count, 1,
        "Alice should NOT be able to delete Bob's document - it should still exist"
    );

    // Switch back to Alice to delete her own document
    set_session_user_id(&user_alice);

    // Test: Alice can delete her own document
    diesel::delete(documents::table.filter(documents::id.eq(alice_doc_id.as_bytes().to_vec())))
        .execute(&mut connection)?;

    // Verify Alice's document was deleted (Alice can't see it anymore)
    let alice_count = documents::table
        .filter(documents::id.eq(alice_doc_id.as_bytes().to_vec()))
        .count()
        .get_result::<i64>(&mut connection)?;
    assert_eq!(
        alice_count, 0,
        "Alice should be able to delete her own document - it should no longer exist"
    );

    Ok(())
}

/// Test that a policy with FOR ALL command works the same as individual
/// policies. FOR ALL applies to SELECT, INSERT, UPDATE, and DELETE operations.
#[test]
#[allow(clippy::too_many_lines)]
fn test_rls_all_policy_works_like_individual_policies() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_all_policy.sql");

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

    // Snapshot the translated SQL for consistency checking
    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("rls_all_policy_translation", translated_sql);

    let mut connection = establish_connection();

    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    let user_alice = Uuid::new_v4();
    let user_bob = Uuid::new_v4();

    // Test SELECT: Each user can only see their own documents
    set_session_user_id(&user_alice);
    let alice_doc_id = Uuid::new_v4();
    diesel::insert_into(documents::table)
        .values(Document {
            id: alice_doc_id.as_bytes().to_vec(),
            owner_id: user_alice.as_bytes().to_vec(),
            title: "Alice Doc".to_string(),
            content: "Alice content".to_string(),
        })
        .execute(&mut connection)?;

    set_session_user_id(&user_bob);
    let bob_doc_id = Uuid::new_v4();
    diesel::insert_into(documents::table)
        .values(Document {
            id: bob_doc_id.as_bytes().to_vec(),
            owner_id: user_bob.as_bytes().to_vec(),
            title: "Bob Doc".to_string(),
            content: "Bob content".to_string(),
        })
        .execute(&mut connection)?;

    // Alice sees only her doc
    set_session_user_id(&user_alice);
    let alice_count = documents::table.count().get_result::<i64>(&mut connection)?;
    assert_eq!(alice_count, 1, "Alice should see only her own document");

    // Bob sees only his doc
    set_session_user_id(&user_bob);
    let bob_count = documents::table.count().get_result::<i64>(&mut connection)?;
    assert_eq!(bob_count, 1, "Bob should see only his own document");

    // Test INSERT: Cannot insert with wrong owner_id
    set_session_user_id(&user_alice);
    let fake_doc_id = Uuid::new_v4();
    let result = diesel::insert_into(documents::table)
        .values(Document {
            id: fake_doc_id.as_bytes().to_vec(),
            owner_id: user_bob.as_bytes().to_vec(), // Wrong owner!
            title: "Fake Doc".to_string(),
            content: "Should fail".to_string(),
        })
        .execute(&mut connection);
    assert!(result.is_err(), "Alice should NOT be able to insert a document with Bob's owner_id");

    // Test UPDATE: Cannot update other user's document
    diesel::update(documents::table.filter(documents::id.eq(bob_doc_id.as_bytes().to_vec())))
        .set(documents::title.eq("Hacked!"))
        .execute(&mut connection)?;

    set_session_user_id(&user_bob);
    let bob_doc = documents::table
        .filter(documents::id.eq(bob_doc_id.as_bytes().to_vec()))
        .select(Document::as_select())
        .first(&mut connection)?;
    assert_eq!(bob_doc.title, "Bob Doc", "Alice should NOT be able to update Bob's document");

    // Test DELETE: Cannot delete other user's document
    set_session_user_id(&user_alice);
    diesel::delete(documents::table.filter(documents::id.eq(bob_doc_id.as_bytes().to_vec())))
        .execute(&mut connection)?;

    set_session_user_id(&user_bob);
    let bob_still_exists = documents::table
        .filter(documents::id.eq(bob_doc_id.as_bytes().to_vec()))
        .count()
        .get_result::<i64>(&mut connection)?;
    assert_eq!(bob_still_exists, 1, "Alice should NOT be able to delete Bob's document");

    // But users CAN update and delete their own documents
    set_session_user_id(&user_alice);
    diesel::update(documents::table.filter(documents::id.eq(alice_doc_id.as_bytes().to_vec())))
        .set(documents::title.eq("Updated Alice Doc"))
        .execute(&mut connection)?;

    let alice_doc = documents::table
        .filter(documents::id.eq(alice_doc_id.as_bytes().to_vec()))
        .select(Document::as_select())
        .first(&mut connection)?;
    assert_eq!(
        alice_doc.title, "Updated Alice Doc",
        "Alice should be able to update her own document"
    );

    diesel::delete(documents::table.filter(documents::id.eq(alice_doc_id.as_bytes().to_vec())))
        .execute(&mut connection)?;

    let alice_deleted = documents::table
        .filter(documents::id.eq(alice_doc_id.as_bytes().to_vec()))
        .count()
        .get_result::<i64>(&mut connection)?;
    assert_eq!(alice_deleted, 0, "Alice should be able to delete her own document");

    Ok(())
}

#[test]
fn test_missing_session_variable_mapping_error() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_basic.sql");

    // Configure WITHOUT the required session variable mapping
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_v7_function_name("uuidv7")
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string());
    // Note: NOT calling .with_session_variable()

    // Translation should fail with SessionVariableMappingNotFound error
    let result = Pg2Sqlite::default().sql(sql)?.translate(&options);

    assert!(
        result.is_err(),
        "Translation should fail when session variable mapping is not configured"
    );

    let error = result.unwrap_err();
    let error_message = error.to_string();
    assert!(
        error_message.contains("session variable mapping")
            || error_message.contains("current_setting"),
        "Error should mention missing session variable mapping, got: {error_message}"
    );

    Ok(())
}
