//! Test for complex trigger with SELECT INTO and multiple IF blocks.
//!
//! Key features tested:
//! - SELECT INTO to fetch context from related tables
//! - Multiple sequential IF NOT EXISTS blocks
//! - Each IF block creates records with context from SELECT INTO

mod helpers;

use diesel::{prelude::*, sqlite::SqliteConnection};
use helpers::{Count, establish_connection_base, uuidv7_impl};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation},
    traits::TranslationOptions,
};

#[declare_sql_function]
extern "SQL" {
    /// Generates a UUIDv7 value.
    fn uuidv7() -> diesel::sql_types::Binary;
}

fn establish_connection() -> SqliteConnection {
    let connection = establish_connection_base();
    uuidv7_utils::register_impl(&connection, uuidv7_impl).expect("Failed to register uuidv7");
    connection
}

/// Test a trigger that:
/// 1. Fetches context via SELECT INTO from related tables
/// 2. Uses that context in an INSERT statement
#[test]
#[allow(clippy::too_many_lines)]
fn test_trigger_with_select_into() -> Result<(), Box<dyn std::error::Error>> {
    #[derive(QueryableByName, Debug)]
    struct TaskLogInfo {
        #[diesel(sql_type = diesel::sql_types::Blob)]
        project_owner_id: Vec<u8>,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        project_priority: i32,
    }

    let sql = r"
-- Projects table with metadata
CREATE TABLE projects (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    name TEXT NOT NULL,
    owner_id UUID NOT NULL,
    priority INTEGER NOT NULL DEFAULT 1
);

-- Tasks table referencing projects
CREATE TABLE tasks (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title TEXT NOT NULL
);

-- Task log to record creation with context from project
CREATE TABLE task_logs (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    task_id UUID NOT NULL,
    project_owner_id UUID NOT NULL,
    project_priority INTEGER NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Trigger: When a task is created, log it with context from the project
CREATE OR REPLACE FUNCTION log_task_creation() RETURNS TRIGGER LANGUAGE plpgsql AS $$ 
DECLARE
    v_log_id UUID;
    v_owner_id UUID;
    v_priority INTEGER;
BEGIN 
    -- Fetch context from the project
    SELECT p.owner_id, p.priority
    INTO v_owner_id, v_priority
    FROM projects p
    WHERE p.id = NEW.project_id;

    -- Create log entry if not exists
    IF NOT EXISTS (
        SELECT 1 FROM task_logs WHERE task_id = NEW.id
    ) THEN
        v_log_id := uuidv7();
        INSERT INTO task_logs (id, task_id, project_owner_id, project_priority)
        VALUES (v_log_id, NEW.id, v_owner_id, v_priority);
    END IF;
    
    RETURN NEW;
END;
$$;

CREATE TRIGGER after_insert_tasks 
AFTER INSERT ON tasks 
FOR EACH ROW EXECUTE FUNCTION log_task_creation();
";

    // Translate the SQL
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string());
    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    // Snapshot the translated SQL for consistency checking
    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("trigger_with_select_into", translated_sql);

    let mut connection = establish_connection();

    // Run the translations
    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    // Setup: Create a project
    let project_id = uuid::Uuid::new_v4();
    let owner_id = uuid::Uuid::new_v4();
    let priority = 5;

    diesel::sql_query(
        "INSERT INTO projects (id, name, owner_id, priority) VALUES (?, 'Test Project', ?, ?)",
    )
    .bind::<diesel::sql_types::Binary, _>(project_id.as_bytes().to_vec())
    .bind::<diesel::sql_types::Binary, _>(owner_id.as_bytes().to_vec())
    .bind::<diesel::sql_types::Integer, _>(priority)
    .execute(&mut connection)?;

    // Verify setup
    let project_count: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM projects").get_result(&mut connection)?;
    assert_eq!(project_count.count, 1, "Should have 1 project");

    let task_log_count_before: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM task_logs").get_result(&mut connection)?;
    assert_eq!(task_log_count_before.count, 0, "Should have 0 task logs before");

    // TEST: Insert a task - trigger should create a task_log with context from
    // project
    let task_id = uuid::Uuid::new_v4();
    diesel::sql_query("INSERT INTO tasks (id, project_id, title) VALUES (?, ?, 'Test Task')")
        .bind::<diesel::sql_types::Binary, _>(task_id.as_bytes().to_vec())
        .bind::<diesel::sql_types::Binary, _>(project_id.as_bytes().to_vec())
        .execute(&mut connection)?;

    // Verify: Task log should be created with context from project
    let task_log_count_after: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM task_logs").get_result(&mut connection)?;
    assert_eq!(task_log_count_after.count, 1, "Trigger should have created 1 task log");

    // Verify the context was properly captured
    let task_log_info: Vec<TaskLogInfo> = diesel::sql_query(
        "SELECT project_owner_id, project_priority FROM task_logs WHERE task_id = ?",
    )
    .bind::<diesel::sql_types::Binary, _>(task_id.as_bytes().to_vec())
    .load(&mut connection)?;

    assert_eq!(task_log_info.len(), 1, "Should find the task log");
    assert_eq!(
        task_log_info[0].project_owner_id,
        owner_id.as_bytes().to_vec(),
        "Owner ID should match project's owner"
    );
    assert_eq!(
        task_log_info[0].project_priority, priority,
        "Priority should match project's priority"
    );

    // TEST 2: Insert another task for the same project
    let task_id_2 = uuid::Uuid::new_v4();
    diesel::sql_query("INSERT INTO tasks (id, project_id, title) VALUES (?, ?, 'Test Task 2')")
        .bind::<diesel::sql_types::Binary, _>(task_id_2.as_bytes().to_vec())
        .bind::<diesel::sql_types::Binary, _>(project_id.as_bytes().to_vec())
        .execute(&mut connection)?;

    let task_log_count_final: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM task_logs").get_result(&mut connection)?;
    assert_eq!(task_log_count_final.count, 2, "Should have 2 task logs");

    Ok(())
}

/// Test a more complex scenario with multiple IF NOT EXISTS blocks.
#[test]
#[allow(clippy::too_many_lines)]
fn test_trigger_with_multiple_if_blocks() -> Result<(), Box<dyn std::error::Error>> {
    #[derive(QueryableByName, Debug)]
    struct AuditInfo {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        org_level: i32,
        #[diesel(sql_type = diesel::sql_types::Text)]
        action: String,
    }

    let sql = r"
-- Organizations with hierarchy levels
CREATE TABLE organizations (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    name TEXT NOT NULL,
    level INTEGER NOT NULL DEFAULT 1
);

-- Teams belong to organizations
CREATE TABLE teams (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name TEXT NOT NULL
);

-- Members can belong to multiple teams
CREATE TABLE members (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    name TEXT NOT NULL
);

-- Team membership (many-to-many)
CREATE TABLE team_members (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    team_id UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    member_id UUID NOT NULL REFERENCES members(id) ON DELETE CASCADE,
    UNIQUE(team_id, member_id)
);

-- Audit log for team membership changes
CREATE TABLE membership_audit (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    team_id UUID NOT NULL,
    member_id UUID NOT NULL,
    org_level INTEGER NOT NULL,
    action TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Trigger: When a member is added to a team, audit with org context
CREATE OR REPLACE FUNCTION audit_team_membership() RETURNS TRIGGER LANGUAGE plpgsql AS $$ 
DECLARE
    v_org_level INTEGER;
    v_org_id UUID;
BEGIN 
    -- Fetch organization context from the team
    SELECT o.level, o.id
    INTO v_org_level, v_org_id
    FROM organizations o
    JOIN teams t ON t.org_id = o.id
    WHERE t.id = NEW.team_id;

    -- Create 'added' audit entry if not exists
    IF NOT EXISTS (
        SELECT 1 FROM membership_audit 
        WHERE team_id = NEW.team_id AND member_id = NEW.member_id AND action = 'added'
    ) THEN
        INSERT INTO membership_audit (id, team_id, member_id, org_level, action)
        VALUES (uuidv7(), NEW.team_id, NEW.member_id, v_org_level, 'added');
    END IF;

    -- Also create a 'verified' audit entry for compliance
    IF NOT EXISTS (
        SELECT 1 FROM membership_audit 
        WHERE team_id = NEW.team_id AND member_id = NEW.member_id AND action = 'verified'
    ) THEN
        INSERT INTO membership_audit (id, team_id, member_id, org_level, action)
        VALUES (uuidv7(), NEW.team_id, NEW.member_id, v_org_level, 'verified');
    END IF;
    
    RETURN NEW;
END;
$$;

CREATE TRIGGER after_insert_team_members 
AFTER INSERT ON team_members 
FOR EACH ROW EXECUTE FUNCTION audit_team_membership();
";

    // Translate the SQL
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_string());
    let translated_migrations = Pg2Sqlite::default().sql(sql)?.translate(&options)?;

    // Snapshot the translated SQL for consistency checking
    let translated_sql =
        translated_migrations.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    insta::assert_snapshot!("trigger_with_multiple_if_blocks", translated_sql);

    let mut connection = establish_connection();

    // Run the translations
    for translated_migration in &translated_migrations {
        let sql_stmt = translated_migration.to_string();
        diesel::sql_query(&sql_stmt).execute(&mut connection)?;
    }

    // Setup: Create org -> team -> member
    let org_id = uuid::Uuid::new_v4();
    let team_id = uuid::Uuid::new_v4();
    let member_id = uuid::Uuid::new_v4();
    let org_level = 3;

    diesel::sql_query("INSERT INTO organizations (id, name, level) VALUES (?, 'Test Org', ?)")
        .bind::<diesel::sql_types::Binary, _>(org_id.as_bytes().to_vec())
        .bind::<diesel::sql_types::Integer, _>(org_level)
        .execute(&mut connection)?;

    diesel::sql_query("INSERT INTO teams (id, org_id, name) VALUES (?, ?, 'Test Team')")
        .bind::<diesel::sql_types::Binary, _>(team_id.as_bytes().to_vec())
        .bind::<diesel::sql_types::Binary, _>(org_id.as_bytes().to_vec())
        .execute(&mut connection)?;

    diesel::sql_query("INSERT INTO members (id, name) VALUES (?, 'Test Member')")
        .bind::<diesel::sql_types::Binary, _>(member_id.as_bytes().to_vec())
        .execute(&mut connection)?;

    // Verify: No audit entries yet
    let audit_count_before: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM membership_audit")
            .get_result(&mut connection)?;
    assert_eq!(audit_count_before.count, 0, "Should have 0 audit entries before");

    // TEST: Add member to team - should trigger 2 audit entries (added + verified)
    let membership_id = uuid::Uuid::new_v4();
    diesel::sql_query("INSERT INTO team_members (id, team_id, member_id) VALUES (?, ?, ?)")
        .bind::<diesel::sql_types::Binary, _>(membership_id.as_bytes().to_vec())
        .bind::<diesel::sql_types::Binary, _>(team_id.as_bytes().to_vec())
        .bind::<diesel::sql_types::Binary, _>(member_id.as_bytes().to_vec())
        .execute(&mut connection)?;

    // Verify: Should have 2 audit entries
    let audit_count_after: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM membership_audit")
            .get_result(&mut connection)?;
    assert_eq!(audit_count_after.count, 2, "Trigger should have created 2 audit entries");

    // Verify: Both entries have correct org_level from SELECT INTO
    let audit_entries: Vec<AuditInfo> =
        diesel::sql_query("SELECT org_level, action FROM membership_audit ORDER BY action")
            .load(&mut connection)?;

    assert_eq!(audit_entries.len(), 2);

    // 'added' entry
    assert_eq!(audit_entries[0].action, "added");
    assert_eq!(audit_entries[0].org_level, org_level);

    // 'verified' entry
    assert_eq!(audit_entries[1].action, "verified");
    assert_eq!(audit_entries[1].org_level, org_level);

    // TEST 2: Try to add the same member again (should be blocked by UNIQUE)
    let result =
        diesel::sql_query("INSERT INTO team_members (id, team_id, member_id) VALUES (?, ?, ?)")
            .bind::<diesel::sql_types::Binary, _>(uuid::Uuid::new_v4().as_bytes().to_vec())
            .bind::<diesel::sql_types::Binary, _>(team_id.as_bytes().to_vec())
            .bind::<diesel::sql_types::Binary, _>(member_id.as_bytes().to_vec())
            .execute(&mut connection);

    assert!(result.is_err(), "Should fail due to UNIQUE constraint");

    // Audit count should still be 2
    let audit_count_final: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM membership_audit")
            .get_result(&mut connection)?;
    assert_eq!(audit_count_final.count, 2, "Should still have 2 audit entries");

    // TEST 3: Add a different member to the same team
    let member_id_2 = uuid::Uuid::new_v4();
    diesel::sql_query("INSERT INTO members (id, name) VALUES (?, 'Test Member 2')")
        .bind::<diesel::sql_types::Binary, _>(member_id_2.as_bytes().to_vec())
        .execute(&mut connection)?;

    let membership_id_2 = uuid::Uuid::new_v4();
    diesel::sql_query("INSERT INTO team_members (id, team_id, member_id) VALUES (?, ?, ?)")
        .bind::<diesel::sql_types::Binary, _>(membership_id_2.as_bytes().to_vec())
        .bind::<diesel::sql_types::Binary, _>(team_id.as_bytes().to_vec())
        .bind::<diesel::sql_types::Binary, _>(member_id_2.as_bytes().to_vec())
        .execute(&mut connection)?;

    // Should now have 4 audit entries (2 per member)
    let audit_count_final_2: Count =
        diesel::sql_query("SELECT COUNT(*) as count FROM membership_audit")
            .get_result(&mut connection)?;
    assert_eq!(audit_count_final_2.count, 4, "Should have 4 audit entries total");

    Ok(())
}
