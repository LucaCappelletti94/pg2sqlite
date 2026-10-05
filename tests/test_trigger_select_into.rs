//! Test for complex trigger with SELECT INTO and multiple IF blocks.
//!
//! Key features tested:
//! - SELECT INTO to fetch context from related tables
//! - Multiple sequential IF NOT EXISTS blocks
//! - Each IF block creates records with context from SELECT INTO

mod helpers;

use diesel::prelude::*;
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};
use rosetta_uuid::Uuid;

// Schema definitions for test 1: Projects and Tasks with logging
diesel::table! {
    /// Projects table with metadata.
    projects (id) {
        /// Project ID.
        id -> Binary,
        /// Project name.
        name -> Text,
        /// Owner ID.
        owner_id -> Binary,
        /// Project priority.
        priority -> Integer,
    }
}

diesel::table! {
    /// Tasks table referencing projects.
    tasks (id) {
        /// Task ID.
        id -> Binary,
        /// Associated project ID.
        project_id -> Binary,
        /// Task title.
        title -> Text,
    }
}

diesel::table! {
    /// Task log to record creation with context from project.
    task_logs (id) {
        /// Log entry ID.
        id -> Binary,
        /// Associated task ID.
        task_id -> Binary,
        /// Project owner ID (captured from project).
        project_owner_id -> Binary,
        /// Project priority (captured from project).
        project_priority -> Integer,
        /// Creation timestamp.
        created_at -> Nullable<Timestamp>,
    }
}

diesel::allow_tables_to_appear_in_same_query!(projects, tasks, task_logs);

// Schema definitions for test 2: Organizations and Teams with audit logging
diesel::table! {
    /// Organizations with hierarchy levels.
    organizations (id) {
        /// Organization ID.
        id -> Binary,
        /// Organization name.
        name -> Text,
        /// Hierarchy level.
        level -> Integer,
    }
}

diesel::table! {
    /// Teams belong to organizations.
    teams (id) {
        /// Team ID.
        id -> Binary,
        /// Associated organization ID.
        org_id -> Binary,
        /// Team name.
        name -> Text,
    }
}

diesel::table! {
    /// Members can belong to multiple teams.
    members (id) {
        /// Member ID.
        id -> Binary,
        /// Member name.
        name -> Text,
    }
}

diesel::table! {
    /// Team membership (many-to-many).
    team_members (id) {
        /// Membership ID.
        id -> Binary,
        /// Team ID.
        team_id -> Binary,
        /// Member ID.
        member_id -> Binary,
    }
}

diesel::table! {
    /// Audit log for team membership changes.
    membership_audit (id) {
        /// Audit entry ID.
        id -> Binary,
        /// Team ID.
        team_id -> Binary,
        /// Member ID.
        member_id -> Binary,
        /// Organization level (captured from organization).
        org_level -> Integer,
        /// Action type (added, verified, etc.).
        action -> Text,
        /// Creation timestamp.
        created_at -> Nullable<Timestamp>,
    }
}

diesel::allow_tables_to_appear_in_same_query!(
    organizations,
    teams,
    members,
    team_members,
    membership_audit
);

/// A project with metadata.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = projects)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Project {
    /// Project ID.
    id: Vec<u8>,
    /// Project name.
    name: String,
    /// Owner ID.
    owner_id: Vec<u8>,
    /// Project priority.
    priority: i32,
}

/// A task referencing a project.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = tasks)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Task {
    /// Task ID.
    id: Vec<u8>,
    /// Associated project ID.
    project_id: Vec<u8>,
    /// Task title.
    title: String,
}

/// An organization with hierarchy level.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = organizations)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Organization {
    /// Organization ID.
    id: Vec<u8>,
    /// Organization name.
    name: String,
    /// Hierarchy level.
    level: i32,
}

/// A team belonging to an organization.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = teams)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Team {
    /// Team ID.
    id: Vec<u8>,
    /// Associated organization ID.
    org_id: Vec<u8>,
    /// Team name.
    name: String,
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = members)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Member {
    /// Member ID.
    id: Vec<u8>,
    /// Member name.
    name: String,
}

/// A team membership record.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = team_members)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct TeamMember {
    /// Membership ID.
    id: Vec<u8>,
    /// Team ID.
    team_id: Vec<u8>,
    /// Member ID.
    member_id: Vec<u8>,
}

/// Test a trigger that:
/// 1. Fetches context via SELECT INTO from related tables
/// 2. Uses that context in an INSERT statement
#[test]
#[allow(clippy::too_many_lines)]
fn test_trigger_with_select_into() -> Result<(), Box<dyn std::error::Error>> {
    /// Task log info for querying captured context.
    #[derive(Queryable, Selectable)]
    #[diesel(table_name = task_logs)]
    #[diesel(check_for_backend(diesel::sqlite::Sqlite))]
    struct TaskLogInfo {
        /// Project owner ID.
        project_owner_id: Vec<u8>,
        /// Project priority.
        project_priority: i32,
    }

    let sql = "
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
        .with_uuid_v7_function_name("uuidv7");
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
    let project_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let priority = 5;

    diesel::insert_into(projects::table)
        .values(&Project {
            id: project_id.as_bytes().to_vec(),
            name: "Test Project".to_string(),
            owner_id: owner_id.as_bytes().to_vec(),
            priority,
        })
        .execute(&mut connection)?;

    // Verify setup
    let project_count: i64 = projects::table.count().get_result(&mut connection)?;
    assert_eq!(project_count, 1, "Should have 1 project");

    let task_log_count_before: i64 = task_logs::table.count().get_result(&mut connection)?;
    assert_eq!(task_log_count_before, 0, "Should have 0 task logs before");

    // TEST: Insert a task - trigger should create a task_log with context from
    // project
    let task_id = Uuid::new_v4();
    diesel::insert_into(tasks::table)
        .values(&Task {
            id: task_id.as_bytes().to_vec(),
            project_id: project_id.as_bytes().to_vec(),
            title: "Test Task".to_string(),
        })
        .execute(&mut connection)?;

    // Verify: Task log should be created with context from project
    let task_log_count_after: i64 = task_logs::table.count().get_result(&mut connection)?;
    assert_eq!(task_log_count_after, 1, "Trigger should have created 1 task log");

    // Verify the context was properly captured
    let task_log_info: Vec<TaskLogInfo> = task_logs::table
        .filter(task_logs::task_id.eq(task_id.as_bytes().to_vec()))
        .select((task_logs::project_owner_id, task_logs::project_priority))
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
    let task_id_2 = Uuid::new_v4();
    diesel::insert_into(tasks::table)
        .values(&Task {
            id: task_id_2.as_bytes().to_vec(),
            project_id: project_id.as_bytes().to_vec(),
            title: "Test Task 2".to_string(),
        })
        .execute(&mut connection)?;

    let task_log_count_final: i64 = task_logs::table.count().get_result(&mut connection)?;
    assert_eq!(task_log_count_final, 2, "Should have 2 task logs");

    Ok(())
}

#[test]
#[allow(clippy::too_many_lines)]
fn test_trigger_with_multiple_if_blocks() -> Result<(), Box<dyn std::error::Error>> {
    /// Audit info for querying captured organization context.
    #[derive(Queryable, Selectable)]
    #[diesel(table_name = membership_audit)]
    #[diesel(check_for_backend(diesel::sqlite::Sqlite))]
    struct AuditInfo {
        /// Organization level.
        org_level: i32,
        /// Action type.
        action: String,
    }

    let sql = "
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
        .with_uuid_v7_function_name("uuidv7");
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
    let org_id = Uuid::new_v4();
    let team_id = Uuid::new_v4();
    let member_id = Uuid::new_v4();
    let org_level = 3;

    diesel::insert_into(organizations::table)
        .values(&Organization {
            id: org_id.as_bytes().to_vec(),
            name: "Test Org".to_string(),
            level: org_level,
        })
        .execute(&mut connection)?;

    diesel::insert_into(teams::table)
        .values(&Team {
            id: team_id.as_bytes().to_vec(),
            org_id: org_id.as_bytes().to_vec(),
            name: "Test Team".to_string(),
        })
        .execute(&mut connection)?;

    diesel::insert_into(members::table)
        .values(&Member { id: member_id.as_bytes().to_vec(), name: "Test Member".to_string() })
        .execute(&mut connection)?;

    // Verify: No audit entries yet
    let audit_count_before: i64 = membership_audit::table.count().get_result(&mut connection)?;
    assert_eq!(audit_count_before, 0, "Should have 0 audit entries before");

    // TEST: Add member to team - should trigger 2 audit entries (added +
    // verified)
    let membership_id = Uuid::new_v4();
    diesel::insert_into(team_members::table)
        .values(&TeamMember {
            id: membership_id.as_bytes().to_vec(),
            team_id: team_id.as_bytes().to_vec(),
            member_id: member_id.as_bytes().to_vec(),
        })
        .execute(&mut connection)?;

    // Verify: Should have 2 audit entries
    let audit_count_after: i64 = membership_audit::table.count().get_result(&mut connection)?;
    assert_eq!(audit_count_after, 2, "Trigger should have created 2 audit entries");

    // Verify: Both entries have correct org_level from SELECT INTO
    let audit_entries: Vec<AuditInfo> = membership_audit::table
        .select((membership_audit::org_level, membership_audit::action))
        .order(membership_audit::action)
        .load(&mut connection)?;

    assert_eq!(audit_entries.len(), 2);

    // 'added' entry
    assert_eq!(audit_entries[0].action, "added");
    assert_eq!(audit_entries[0].org_level, org_level);

    // 'verified' entry
    assert_eq!(audit_entries[1].action, "verified");
    assert_eq!(audit_entries[1].org_level, org_level);

    // TEST 2: Try to add the same member again (should be blocked by UNIQUE)
    let result = diesel::insert_into(team_members::table)
        .values(&TeamMember {
            id: Uuid::new_v4().as_bytes().to_vec(),
            team_id: team_id.as_bytes().to_vec(),
            member_id: member_id.as_bytes().to_vec(),
        })
        .execute(&mut connection);

    assert!(result.is_err(), "Should fail due to UNIQUE constraint");

    // Audit count should still be 2
    let audit_count_final: i64 = membership_audit::table.count().get_result(&mut connection)?;
    assert_eq!(audit_count_final, 2, "Should still have 2 audit entries");

    // TEST 3: Add a different member to the same team
    let member_id_2 = Uuid::new_v4();
    diesel::insert_into(members::table)
        .values(&Member { id: member_id_2.as_bytes().to_vec(), name: "Test Member 2".to_string() })
        .execute(&mut connection)?;

    let membership_id_2 = Uuid::new_v4();
    diesel::insert_into(team_members::table)
        .values(&TeamMember {
            id: membership_id_2.as_bytes().to_vec(),
            team_id: team_id.as_bytes().to_vec(),
            member_id: member_id_2.as_bytes().to_vec(),
        })
        .execute(&mut connection)?;

    // Should now have 4 audit entries (2 per member)
    let audit_count_final_2: i64 = membership_audit::table.count().get_result(&mut connection)?;
    assert_eq!(audit_count_final_2, 4, "Should have 4 audit entries total");

    Ok(())
}
