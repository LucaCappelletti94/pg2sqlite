//! Test for multi-tenant isolation RLS pattern.
//!
//! Scenario: Users belong to one or more tenants via tenant_users table.
//! They can only access projects within tenants they belong to.
//! Delete operations require admin role within the tenant.

mod helpers;

use diesel::prelude::*;
use helpers::establish_connection;
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, UuidRepresentation},
    traits::TranslationOptions,
};
use rosetta_uuid::Uuid;

use crate::helpers::set_session_user_id;

mod schema {
    diesel::table! {
        /// Tenants table (no RLS - accessible to all).
        tenants (id) {
            id -> Binary,
            name -> Text,
        }
    }

    diesel::table! {
        /// Tenant users mapping (no RLS - accessible to all).
        tenant_users (id) {
            id -> Binary,
            tenant_id -> Binary,
            user_id -> Binary,
            role -> Text,
        }
    }

    diesel::table! {
        /// Projects view with RLS filtering based on tenant membership.
        projects (id) {
            id -> Binary,
            tenant_id -> Binary,
            name -> Text,
            created_by -> Binary,
        }
    }
}

use schema::{projects, tenant_users, tenants};

#[derive(Queryable, Insertable, Selectable, Debug)]
#[diesel(table_name = tenants)]
struct Tenant {
    id: Vec<u8>,
    name: String,
}

#[derive(Queryable, Insertable, Selectable, Debug)]
#[diesel(table_name = tenant_users)]
struct TenantUser {
    id: Vec<u8>,
    tenant_id: Vec<u8>,
    user_id: Vec<u8>,
    role: String,
}

#[derive(Queryable, Insertable, Selectable, Debug)]
#[diesel(table_name = projects)]
struct Project {
    id: Vec<u8>,
    tenant_id: Vec<u8>,
    name: String,
    created_by: Vec<u8>,
}

fn translate_and_setup()
-> Result<(diesel::SqliteConnection, Pg2SqliteOptions), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_tenant_isolation.sql");

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

    Ok((connection, options))
}

#[test]
fn test_tenant_isolation_translation_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let sql = include_str!("fixtures/rls_tenant_isolation.sql");

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
    insta::assert_snapshot!("rls_tenant_isolation_translation", translated_sql);

    Ok(())
}

#[test]
fn test_user_sees_own_tenant_only() -> Result<(), Box<dyn std::error::Error>> {
    let (mut connection, _) = translate_and_setup()?;

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let user_alice = Uuid::new_v4();
    let user_bob = Uuid::new_v4();

    // Create tenants (no RLS, so use direct table name)
    diesel::insert_into(tenants::table)
        .values(Tenant { id: tenant_a.as_bytes().to_vec(), name: "Tenant A".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(tenants::table)
        .values(Tenant { id: tenant_b.as_bytes().to_vec(), name: "Tenant B".to_string() })
        .execute(&mut connection)?;

    // Alice belongs to Tenant A
    diesel::insert_into(tenant_users::table)
        .values(TenantUser {
            id: Uuid::new_v4().as_bytes().to_vec(),
            tenant_id: tenant_a.as_bytes().to_vec(),
            user_id: user_alice.as_bytes().to_vec(),
            role: "member".to_string(),
        })
        .execute(&mut connection)?;

    // Bob belongs to Tenant B
    diesel::insert_into(tenant_users::table)
        .values(TenantUser {
            id: Uuid::new_v4().as_bytes().to_vec(),
            tenant_id: tenant_b.as_bytes().to_vec(),
            user_id: user_bob.as_bytes().to_vec(),
            role: "member".to_string(),
        })
        .execute(&mut connection)?;

    // Create a project in Tenant A (Alice's project)
    set_session_user_id(&user_alice);
    diesel::insert_into(projects::table)
        .values(Project {
            id: Uuid::new_v4().as_bytes().to_vec(),
            tenant_id: tenant_a.as_bytes().to_vec(),
            name: "Project Alpha".to_string(),
            created_by: user_alice.as_bytes().to_vec(),
        })
        .execute(&mut connection)?;

    // Create a project in Tenant B (Bob's project)
    set_session_user_id(&user_bob);
    diesel::insert_into(projects::table)
        .values(Project {
            id: Uuid::new_v4().as_bytes().to_vec(),
            tenant_id: tenant_b.as_bytes().to_vec(),
            name: "Project Beta".to_string(),
            created_by: user_bob.as_bytes().to_vec(),
        })
        .execute(&mut connection)?;

    // Alice should see only Project Alpha
    set_session_user_id(&user_alice);
    let alice_projects = projects::table.select(Project::as_select()).load(&mut connection)?;
    assert_eq!(alice_projects.len(), 1, "Alice should see 1 project");
    assert_eq!(alice_projects[0].name, "Project Alpha");

    // Bob should see only Project Beta
    set_session_user_id(&user_bob);
    let bob_projects = projects::table.select(Project::as_select()).load(&mut connection)?;
    assert_eq!(bob_projects.len(), 1, "Bob should see 1 project");
    assert_eq!(bob_projects[0].name, "Project Beta");

    Ok(())
}

#[test]
fn test_user_cannot_see_other_tenant() -> Result<(), Box<dyn std::error::Error>> {
    let (mut connection, _) = translate_and_setup()?;

    let tenant_a = Uuid::new_v4();
    let user_alice = Uuid::new_v4();
    let user_charlie = Uuid::new_v4();

    // Create tenant
    diesel::insert_into(tenants::table)
        .values(Tenant { id: tenant_a.as_bytes().to_vec(), name: "Tenant A".to_string() })
        .execute(&mut connection)?;

    // Alice belongs to Tenant A
    diesel::insert_into(tenant_users::table)
        .values(TenantUser {
            id: Uuid::new_v4().as_bytes().to_vec(),
            tenant_id: tenant_a.as_bytes().to_vec(),
            user_id: user_alice.as_bytes().to_vec(),
            role: "member".to_string(),
        })
        .execute(&mut connection)?;

    // Charlie does NOT belong to any tenant

    // Create a project in Tenant A
    set_session_user_id(&user_alice);
    diesel::insert_into(projects::table)
        .values(Project {
            id: Uuid::new_v4().as_bytes().to_vec(),
            tenant_id: tenant_a.as_bytes().to_vec(),
            name: "Tenant A Project".to_string(),
            created_by: user_alice.as_bytes().to_vec(),
        })
        .execute(&mut connection)?;

    // Charlie should see no projects
    set_session_user_id(&user_charlie);
    let charlie_count = projects::table.count().get_result::<i64>(&mut connection)?;
    assert_eq!(charlie_count, 0, "Charlie should see 0 projects");

    Ok(())
}

#[test]
fn test_multi_tenant_user_sees_both() -> Result<(), Box<dyn std::error::Error>> {
    let (mut connection, _) = translate_and_setup()?;

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let user_multi = Uuid::new_v4();

    // Create tenants
    diesel::insert_into(tenants::table)
        .values(Tenant { id: tenant_a.as_bytes().to_vec(), name: "Tenant A".to_string() })
        .execute(&mut connection)?;
    diesel::insert_into(tenants::table)
        .values(Tenant { id: tenant_b.as_bytes().to_vec(), name: "Tenant B".to_string() })
        .execute(&mut connection)?;

    // User belongs to BOTH tenants
    diesel::insert_into(tenant_users::table)
        .values(TenantUser {
            id: Uuid::new_v4().as_bytes().to_vec(),
            tenant_id: tenant_a.as_bytes().to_vec(),
            user_id: user_multi.as_bytes().to_vec(),
            role: "member".to_string(),
        })
        .execute(&mut connection)?;
    diesel::insert_into(tenant_users::table)
        .values(TenantUser {
            id: Uuid::new_v4().as_bytes().to_vec(),
            tenant_id: tenant_b.as_bytes().to_vec(),
            user_id: user_multi.as_bytes().to_vec(),
            role: "member".to_string(),
        })
        .execute(&mut connection)?;

    // Create projects in each tenant
    set_session_user_id(&user_multi);
    diesel::insert_into(projects::table)
        .values(Project {
            id: Uuid::new_v4().as_bytes().to_vec(),
            tenant_id: tenant_a.as_bytes().to_vec(),
            name: "Project in A".to_string(),
            created_by: user_multi.as_bytes().to_vec(),
        })
        .execute(&mut connection)?;

    diesel::insert_into(projects::table)
        .values(Project {
            id: Uuid::new_v4().as_bytes().to_vec(),
            tenant_id: tenant_b.as_bytes().to_vec(),
            name: "Project in B".to_string(),
            created_by: user_multi.as_bytes().to_vec(),
        })
        .execute(&mut connection)?;

    // User should see both projects
    let project_count = projects::table.count().get_result::<i64>(&mut connection)?;
    assert_eq!(project_count, 2, "Multi-tenant user should see 2 projects");

    Ok(())
}

#[test]
fn test_admin_can_delete() -> Result<(), Box<dyn std::error::Error>> {
    let (mut connection, _) = translate_and_setup()?;

    let tenant_a = Uuid::new_v4();
    let user_admin = Uuid::new_v4();

    // Create tenant
    diesel::insert_into(tenants::table)
        .values(Tenant { id: tenant_a.as_bytes().to_vec(), name: "Tenant A".to_string() })
        .execute(&mut connection)?;

    // Admin in Tenant A
    diesel::insert_into(tenant_users::table)
        .values(TenantUser {
            id: Uuid::new_v4().as_bytes().to_vec(),
            tenant_id: tenant_a.as_bytes().to_vec(),
            user_id: user_admin.as_bytes().to_vec(),
            role: "admin".to_string(),
        })
        .execute(&mut connection)?;

    // Create a project
    set_session_user_id(&user_admin);
    let project_id = Uuid::new_v4();
    diesel::insert_into(projects::table)
        .values(Project {
            id: project_id.as_bytes().to_vec(),
            tenant_id: tenant_a.as_bytes().to_vec(),
            name: "Deletable Project".to_string(),
            created_by: user_admin.as_bytes().to_vec(),
        })
        .execute(&mut connection)?;

    // Admin can delete the project
    diesel::delete(projects::table.filter(projects::id.eq(project_id.as_bytes().to_vec())))
        .execute(&mut connection)?;

    let count = projects::table.count().get_result::<i64>(&mut connection)?;
    assert_eq!(count, 0, "Admin should be able to delete the project");

    Ok(())
}

#[test]
fn test_member_cannot_delete() -> Result<(), Box<dyn std::error::Error>> {
    let (mut connection, _) = translate_and_setup()?;

    let tenant_a = Uuid::new_v4();
    let user_member = Uuid::new_v4();

    // Create tenant
    diesel::insert_into(tenants::table)
        .values(Tenant { id: tenant_a.as_bytes().to_vec(), name: "Tenant A".to_string() })
        .execute(&mut connection)?;

    // Member (not admin) in Tenant A
    diesel::insert_into(tenant_users::table)
        .values(TenantUser {
            id: Uuid::new_v4().as_bytes().to_vec(),
            tenant_id: tenant_a.as_bytes().to_vec(),
            user_id: user_member.as_bytes().to_vec(),
            role: "member".to_string(),
        })
        .execute(&mut connection)?;

    // Create a project
    set_session_user_id(&user_member);
    let project_id = Uuid::new_v4();
    diesel::insert_into(projects::table)
        .values(Project {
            id: project_id.as_bytes().to_vec(),
            tenant_id: tenant_a.as_bytes().to_vec(),
            name: "Protected Project".to_string(),
            created_by: user_member.as_bytes().to_vec(),
        })
        .execute(&mut connection)?;

    // Member attempts to delete - should not actually delete (no matching rows)
    diesel::delete(projects::table.filter(projects::id.eq(project_id.as_bytes().to_vec())))
        .execute(&mut connection)?;

    // Project should still exist
    let count = projects::table.count().get_result::<i64>(&mut connection)?;
    assert_eq!(count, 1, "Member should NOT be able to delete the project");

    Ok(())
}
