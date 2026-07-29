//! Test for RLS with multiple permissive policies per operation.
//!
//! Scenario: Multiple SELECT policies on the same table combine with OR,
//! allowing access if ANY policy matches.

mod helpers;

use diesel::prelude::*;
use helpers::{establish_connection, set_session_department, set_session_user_id};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation},
    traits::TranslationOptions,
};
use rosetta_uuid::Uuid;

mod schema {
    diesel::table! {
        /// Documents view with RLS filtering based on owner, public, or department.
        documents (id) {
            id -> Binary,
            owner_id -> Binary,
            is_public -> Bool,
            department -> Nullable<Text>,
            title -> Text,
        }
    }
}

use schema::documents;

#[derive(Queryable, Insertable, Selectable, Debug, Clone)]
#[diesel(table_name = documents)]
struct Document {
    id: Vec<u8>,
    owner_id: Vec<u8>,
    is_public: bool,
    department: Option<String>,
    title: String,
}

fn translate_and_setup() -> Result<diesel::SqliteConnection, Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_multiple_policies.sql");

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string())
        .with_session_user_role("authenticated".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_department",
            "current_app_department",
        ))
        .with_rls_audit_table_name("rls_audit".to_string());

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let mut connection = establish_connection();

    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    Ok(connection)
}

/// Snapshot test for multiple policies RLS translation.
#[test]
fn test_multiple_policies_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_multiple_policies.sql");

    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string())
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_department",
            "current_app_department",
        ));

    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("rls_multiple_policies_translation", translated_sql);

    Ok(())
}

#[test]
fn test_owner_sees_own_docs() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice = Uuid::new_v4();

    set_session_user_id(&alice);
    set_session_department(None);
    diesel::insert_into(documents::table)
        .values(Document {
            id: Uuid::new_v4().as_bytes().to_vec(),
            owner_id: alice.as_bytes().to_vec(),
            is_public: false,
            department: Some("engineering".to_string()),
            title: "Alice's Private Doc".to_string(),
        })
        .execute(&mut connection)?;

    let visible = documents::table.select(Document::as_select()).load(&mut connection)?;
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].title, "Alice's Private Doc");

    Ok(())
}

#[test]
fn test_anyone_sees_public_docs() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();

    // Alice creates a public document
    set_session_user_id(&alice);
    set_session_department(None);
    diesel::insert_into(documents::table)
        .values(Document {
            id: Uuid::new_v4().as_bytes().to_vec(),
            owner_id: alice.as_bytes().to_vec(),
            is_public: true,
            department: None,
            title: "Public Doc".to_string(),
        })
        .execute(&mut connection)?;

    // Bob (different user, no department) should see the public doc
    set_session_user_id(&bob);
    set_session_department(None);
    let visible = documents::table.select(Document::as_select()).load(&mut connection)?;
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].title, "Public Doc");

    Ok(())
}

#[test]
fn test_dept_member_sees_dept_docs() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();

    // Alice creates an engineering department document
    set_session_user_id(&alice);
    set_session_department(Some("engineering"));
    diesel::insert_into(documents::table)
        .values(Document {
            id: Uuid::new_v4().as_bytes().to_vec(),
            owner_id: alice.as_bytes().to_vec(),
            is_public: false,
            department: Some("engineering".to_string()),
            title: "Engineering Doc".to_string(),
        })
        .execute(&mut connection)?;

    // Bob in engineering department should see it
    set_session_user_id(&bob);
    set_session_department(Some("engineering"));
    let visible = documents::table.select(Document::as_select()).load(&mut connection)?;
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].title, "Engineering Doc");

    // Charlie in sales should NOT see it
    let charlie = Uuid::new_v4();
    set_session_user_id(&charlie);
    set_session_department(Some("sales"));
    let visible = documents::table.select(Document::as_select()).load(&mut connection)?;
    assert_eq!(visible.len(), 0);

    Ok(())
}

#[test]
fn test_user_sees_union_of_all_matching() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();
    let charlie = Uuid::new_v4();

    // Alice creates her private doc
    set_session_user_id(&alice);
    set_session_department(Some("engineering"));
    diesel::insert_into(documents::table)
        .values(Document {
            id: Uuid::new_v4().as_bytes().to_vec(),
            owner_id: alice.as_bytes().to_vec(),
            is_public: false,
            department: None,
            title: "Alice's Private".to_string(),
        })
        .execute(&mut connection)?;

    // Bob creates a public doc
    set_session_user_id(&bob);
    set_session_department(None);
    diesel::insert_into(documents::table)
        .values(Document {
            id: Uuid::new_v4().as_bytes().to_vec(),
            owner_id: bob.as_bytes().to_vec(),
            is_public: true,
            department: None,
            title: "Bob's Public".to_string(),
        })
        .execute(&mut connection)?;

    // Charlie creates an engineering department doc
    set_session_user_id(&charlie);
    set_session_department(Some("engineering"));
    diesel::insert_into(documents::table)
        .values(Document {
            id: Uuid::new_v4().as_bytes().to_vec(),
            owner_id: charlie.as_bytes().to_vec(),
            is_public: false,
            department: Some("engineering".to_string()),
            title: "Engineering Team Doc".to_string(),
        })
        .execute(&mut connection)?;

    // Alice in engineering should see:
    // 1. Her own private doc (owner policy)
    // 2. Bob's public doc (public policy)
    // 3. Charlie's engineering doc (department policy)
    set_session_user_id(&alice);
    set_session_department(Some("engineering"));
    let visible = documents::table.select(Document::as_select()).load(&mut connection)?;
    assert_eq!(visible.len(), 3, "Alice should see 3 documents");

    let titles: Vec<_> = visible.iter().map(|d| d.title.as_str()).collect();
    assert!(titles.contains(&"Alice's Private"));
    assert!(titles.contains(&"Bob's Public"));
    assert!(titles.contains(&"Engineering Team Doc"));

    // Bob with no department should see:
    // 1. His own public doc (owner policy)
    // 2. Alice's private doc? No - it's not public and not his
    // So Bob sees only his public doc
    set_session_user_id(&bob);
    set_session_department(None);
    let visible = documents::table.select(Document::as_select()).load(&mut connection)?;
    assert_eq!(visible.len(), 1, "Bob should see 1 document (his own public)");
    assert_eq!(visible[0].title, "Bob's Public");

    Ok(())
}

/// Multiple PERMISSIVE policies are a union: any single match grants access.
#[test]
fn test_policies_combine_with_or() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice = Uuid::new_v4();
    let doc_id = Uuid::new_v4();

    // Alice creates a doc that matches multiple policies:
    // - She owns it
    // - It's public
    // - It's in her department
    set_session_user_id(&alice);
    set_session_department(Some("engineering"));
    diesel::insert_into(documents::table)
        .values(Document {
            id: doc_id.as_bytes().to_vec(),
            owner_id: alice.as_bytes().to_vec(),
            is_public: true,
            department: Some("engineering".to_string()),
            title: "Triple Match".to_string(),
        })
        .execute(&mut connection)?;

    // Should appear exactly once (not three times)
    let visible = documents::table.select(Document::as_select()).load(&mut connection)?;
    assert_eq!(visible.len(), 1, "Document should appear exactly once");
    assert_eq!(visible[0].title, "Triple Match");

    Ok(())
}

#[test]
fn test_non_matching_sees_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = translate_and_setup()?;

    let alice = Uuid::new_v4();
    let nobody = Uuid::new_v4();

    // Alice creates a private engineering doc
    set_session_user_id(&alice);
    set_session_department(Some("engineering"));
    diesel::insert_into(documents::table)
        .values(Document {
            id: Uuid::new_v4().as_bytes().to_vec(),
            owner_id: alice.as_bytes().to_vec(),
            is_public: false,
            department: Some("engineering".to_string()),
            title: "Private Engineering".to_string(),
        })
        .execute(&mut connection)?;

    // A user in sales (different department), not owner, sees nothing
    set_session_user_id(&nobody);
    set_session_department(Some("sales"));
    let visible = documents::table.select(Document::as_select()).load(&mut connection)?;
    assert_eq!(visible.len(), 0, "Non-matching user should see nothing");

    Ok(())
}
