//! Tests for RLS validation and monitoring.
//!
//! This module tests the RLS validation system that monitors for violations
//! between backend RLS policies and client-side views.

mod helpers;

use diesel::prelude::*;
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation};
use rosetta_uuid::Uuid;

/// PostgreSQL schema with basic RLS setup for testing.
const RLS_SCHEMA: &str = r#"
CREATE TABLE documents (
    id UUID PRIMARY KEY,
    owner_id UUID NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL
);

ALTER TABLE documents ENABLE ROW LEVEL SECURITY;

CREATE POLICY documents_select_policy ON documents
    FOR SELECT
    TO authenticated
    USING (owner_id = current_setting('app.user_id')::uuid);

CREATE POLICY documents_insert_policy ON documents
    FOR INSERT
    TO authenticated
    WITH CHECK (owner_id = current_setting('app.user_id')::uuid);

CREATE POLICY documents_update_policy ON documents
    FOR UPDATE
    TO authenticated
    USING (owner_id = current_setting('app.user_id')::uuid)
    WITH CHECK (owner_id = current_setting('app.user_id')::uuid);

CREATE POLICY documents_delete_policy ON documents
    FOR DELETE
    TO authenticated
    USING (owner_id = current_setting('app.user_id')::uuid);
"#;

/// Test 1: RLS requires audit table name to be configured.
/// This test ensures that if RLS is present but no audit table name is set,
/// the translation fails with a clear error.
#[test]
fn test_rls_requires_audit_table_name() {
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_session_user_role("authenticated".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));
    // Note: NO .with_rls_audit_table_name() called

    let result = Pg2Sqlite::default().sql(RLS_SCHEMA).and_then(|t| t.translate(&options));

    // Should fail with RlsAuditTableNameRequired error
    assert!(result.is_err());
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("audit table name") && err_msg.contains("must be configured"),
        "Expected error about missing audit table name, got: {}",
        err_msg
    );
}

#[test]
fn test_audit_table_uses_configured_name() -> Result<(), Box<dyn std::error::Error>> {
    let custom_audit_name = "my_custom_violations";
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_session_user_role("authenticated".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_rls_audit_table_name(custom_audit_name.to_string());

    let translated = Pg2Sqlite::default().sql(RLS_SCHEMA)?.translate(&options)?;
    let sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    // Should contain CREATE TABLE with the custom name
    assert!(
        sql.contains(&format!("CREATE TABLE IF NOT EXISTS {}", custom_audit_name)),
        "Expected audit table creation with name '{}', got:\n{}",
        custom_audit_name,
        sql
    );

    // Execute the generated DDL against SQLite to verify it runs. The
    // translated statements are arbitrary CREATE TABLE, VIEW, and TRIGGER
    // strings that cannot be expressed via diesel's typed DSL.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for s in &translated {
        conn.execute_batch(&format!("{s};")).unwrap();
    }

    Ok(())
}

#[test]
fn test_triggers_use_configured_audit_table() -> Result<(), Box<dyn std::error::Error>> {
    let custom_audit_name = "custom_audit";
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_session_user_role("authenticated".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_rls_audit_table_name(custom_audit_name.to_string());

    let translated = Pg2Sqlite::default().sql(RLS_SCHEMA)?.translate(&options)?;
    let sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    // Triggers should INSERT INTO the custom audit table
    assert!(
        sql.contains(&format!("INSERT INTO {}", custom_audit_name)),
        "Expected triggers to insert into '{}', got:\n{}",
        custom_audit_name,
        sql
    );

    // Execute the generated DDL against SQLite to verify it runs. The
    // translated statements are arbitrary CREATE TABLE, VIEW, and TRIGGER
    // strings that cannot be expressed via diesel's typed DSL.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for s in &translated {
        conn.execute_batch(&format!("{s};")).unwrap();
    }

    Ok(())
}

mod schema {
    diesel::table! {
        documents (id) {
            id -> Binary,
            owner_id -> Binary,
            title -> Text,
            content -> Text,
        }
    }
}

#[derive(Queryable, Insertable, Selectable, QueryableByName, Debug, PartialEq)]
#[diesel(table_name = schema::documents)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Document {
    id: Vec<u8>,
    owner_id: Vec<u8>,
    title: String,
    content: String,
}

#[test]
fn test_monitor_mode_logs_violations() -> Result<(), Box<dyn std::error::Error>> {
    // Schema with a permissive INSERT (WITH CHECK = true) but a restrictive
    // SELECT USING (owner_id = session_user). Alice can insert a bob-owned row
    // through the view (WITH CHECK passes), but the row is not visible to alice
    // through SELECT USING. The monitoring trigger must log
    // 'select_not_visible' rather than 'rls_policy_violation', matching
    // PostgreSQL semantics.
    let audit_table_name = "rls_violations";
    const MONITOR_SCHEMA: &str = r#"
CREATE TABLE documents (
    id UUID PRIMARY KEY,
    owner_id UUID NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL
);
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY documents_select_policy ON documents
    FOR SELECT
    TO authenticated
    USING (owner_id = current_setting('app.user_id')::uuid);
CREATE POLICY documents_insert_policy ON documents
    FOR INSERT
    TO authenticated
    WITH CHECK (true);
"#;
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_session_user_role("authenticated".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_rls_audit_table_name(audit_table_name.to_string());

    let translated = Pg2Sqlite::default().sql(MONITOR_SCHEMA)?.translate(&options)?;
    let mut conn = establish_connection();
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // Set session user to alice.
    let user_alice = Uuid::new_v4();
    helpers::set_session_user_id(&user_alice);

    // Insert a bob-owned document through the VIEW as alice.
    // WITH CHECK(true) allows it; SELECT USING(owner_id = alice) hides it.
    let user_bob = Uuid::new_v4();
    let doc_id = Uuid::new_v4();
    // Translator output for DML must be applied via sql_query; typed DSL
    // cannot express the view-path insert through translated RLS triggers.
    diesel::sql_query("INSERT INTO documents (id, owner_id, title, content) VALUES (?, ?, ?, ?)")
        .bind::<diesel::sql_types::Binary, _>(doc_id.as_bytes())
        .bind::<diesel::sql_types::Binary, _>(user_bob.as_bytes())
        .bind::<diesel::sql_types::Text, _>("Bob's Secret")
        .bind::<diesel::sql_types::Text, _>("Content")
        .execute(&mut conn)?;

    // Alice cannot see bob's document through the view.
    let visible_docs: Vec<Document> = schema::documents::table.load::<Document>(&mut conn)?;
    assert_eq!(visible_docs.len(), 0, "Alice should not see Bob's document through the view");

    // The monitoring trigger must have logged the SELECT-visibility mismatch.
    #[derive(QueryableByName, Debug)]
    struct ViolationRecord {
        #[diesel(sql_type = diesel::sql_types::Text)]
        table_name: String,
        #[diesel(sql_type = diesel::sql_types::Text)]
        violation_type: String,
    }
    let violations: Vec<ViolationRecord> =
        diesel::sql_query(format!("SELECT table_name, violation_type FROM {audit_table_name}"))
            .load(&mut conn)?;
    assert_eq!(violations.len(), 1, "Should have logged exactly one audit row");
    assert_eq!(violations[0].table_name, "documents");
    // The monitor now reports 'select_not_visible', not 'rls_policy_violation',
    // because a write whose new row is not SELECT-visible is not a policy
    // violation in the PostgreSQL sense.
    assert_eq!(violations[0].violation_type, "select_not_visible");

    Ok(())
}

#[test]
fn test_strict_mode_blocks_violations() -> Result<(), Box<dyn std::error::Error>> {
    let audit_table_name = "rls_violations_strict";
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_session_user_role("authenticated".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_rls_audit_table_name(audit_table_name.to_string())
        .with_strict_rls_validation(); // Enable strict mode

    let translated = Pg2Sqlite::default().sql(RLS_SCHEMA)?.translate(&options)?;
    let sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    // Strict mode should include RAISE(ABORT) in triggers
    assert!(sql.contains("RAISE(ABORT"), "Expected RAISE(ABORT) in strict mode, got:\n{}", sql);

    let mut conn = establish_connection();

    // Execute all translated statements
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // Set up session user
    let user_alice = Uuid::new_v4();
    helpers::set_session_user_id(&user_alice);

    // Try to insert a document directly into the backing table
    let user_bob = Uuid::new_v4();
    let doc_id = Uuid::new_v4();

    let result = diesel::sql_query(
        "INSERT INTO documents_rls (id, owner_id, title, content) VALUES (?, ?, ?, ?)",
    )
    .bind::<diesel::sql_types::Binary, _>(doc_id.as_bytes())
    .bind::<diesel::sql_types::Binary, _>(user_bob.as_bytes())
    .bind::<diesel::sql_types::Text, _>("Bob's Secret")
    .bind::<diesel::sql_types::Text, _>("Content")
    .execute(&mut conn);

    // Should fail with abort error. Either the new BEFORE INSERT guard
    // ("new row violates row-level security policy") or the AFTER INSERT
    // validation trigger ("RLS validation: ...") fires first; both are
    // valid strict-mode aborts.
    assert!(result.is_err(), "Strict mode should abort on violation");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("RLS validation")
            || err.contains("abort")
            || err.contains("row-level security policy"),
        "Expected RLS validation error, got: {}",
        err
    );

    // Verify no data was inserted
    let count: i64 = diesel::sql_query("SELECT COUNT(*) as count FROM documents_rls")
        .get_result::<helpers::Count>(&mut conn)?
        .count;
    assert_eq!(count, 0, "No data should be inserted in strict mode on violation");

    Ok(())
}

#[test]
fn test_validation_views_exist() -> Result<(), Box<dyn std::error::Error>> {
    // Use a schema where INSERT WITH CHECK (true) allows any insert but SELECT
    // USING restricts visibility by owner. Alice inserts alice's doc (visible)
    // and then inserts bob's doc through the view (not visible to alice).
    // The violations view must show only bob's doc.
    const VALIDATION_SCHEMA: &str = r#"
CREATE TABLE documents (
    id UUID PRIMARY KEY,
    owner_id UUID NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL
);
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY documents_select_policy ON documents
    FOR SELECT
    TO authenticated
    USING (owner_id = current_setting('app.user_id')::uuid);
CREATE POLICY documents_insert_policy ON documents
    FOR INSERT
    TO authenticated
    WITH CHECK (true);
"#;
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_session_user_role("authenticated".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_rls_audit_table_name("rls_audit".to_string());

    let translated = Pg2Sqlite::default().sql(VALIDATION_SCHEMA)?.translate(&options)?;
    let sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        sql.contains("CREATE VIEW documents_rls_violations"),
        "Expected validation view creation, got:\n{}",
        sql
    );

    let mut conn = establish_connection();
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    let user_alice = Uuid::new_v4();
    let user_bob = Uuid::new_v4();
    helpers::set_session_user_id(&user_alice);

    // Alice inserts her own doc through the view (WITH CHECK=true, visible to
    // alice).
    let alice_doc_id = Uuid::new_v4();
    // View-path insert; typed DSL cannot express the INSTEAD OF trigger path.
    diesel::sql_query("INSERT INTO documents (id, owner_id, title, content) VALUES (?, ?, ?, ?)")
        .bind::<diesel::sql_types::Binary, _>(alice_doc_id.as_bytes())
        .bind::<diesel::sql_types::Binary, _>(user_alice.as_bytes())
        .bind::<diesel::sql_types::Text, _>("Alice's Doc")
        .bind::<diesel::sql_types::Text, _>("Content")
        .execute(&mut conn)?;

    // Alice inserts a bob-owned doc through the view (WITH CHECK=true, not
    // visible to alice).
    let bob_doc_id = Uuid::new_v4();
    diesel::sql_query("INSERT INTO documents (id, owner_id, title, content) VALUES (?, ?, ?, ?)")
        .bind::<diesel::sql_types::Binary, _>(bob_doc_id.as_bytes())
        .bind::<diesel::sql_types::Binary, _>(user_bob.as_bytes())
        .bind::<diesel::sql_types::Text, _>("Bob's Doc")
        .bind::<diesel::sql_types::Text, _>("Content")
        .execute(&mut conn)?;

    // Violations view: rows in backing table not visible through SELECT USING.
    let violations: Vec<Document> =
        diesel::sql_query("SELECT * FROM documents_rls_violations").load::<Document>(&mut conn)?;

    assert_eq!(violations.len(), 1, "Should have exactly one violation in the view");
    assert_eq!(violations[0].title, "Bob's Doc");

    Ok(())
}

#[test]
fn test_monitor_mode_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_session_user_role("authenticated".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_rls_audit_table_name("rls_violations".to_string());

    let translated = Pg2Sqlite::default().sql(RLS_SCHEMA)?.translate(&options)?;
    let sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    insta::assert_snapshot!("rls_validation_monitor_mode", sql);

    Ok(())
}

#[test]
fn test_strict_mode_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_session_user_role("authenticated".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
        .with_rls_audit_table_name("rls_violations".to_string())
        .with_strict_rls_validation();

    let translated = Pg2Sqlite::default().sql(RLS_SCHEMA)?.translate(&options)?;
    let sql = translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    insta::assert_snapshot!("rls_validation_strict_mode", sql);

    Ok(())
}
