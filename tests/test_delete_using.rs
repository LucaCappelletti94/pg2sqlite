//! Test for DELETE ... USING translation inside triggers.

use diesel::{Connection, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

// Schema definitions for test tables
diesel::table! {
    /// Test table for teams with parent relationships.
    teams (id) {
        /// Team ID.
        id -> Integer,
        /// Parent team ID (nullable).
        parent_team_id -> Nullable<Integer>,
    }
}

diesel::table! {
    /// Test table for team memberships.
    team_members (team_id, member_id) {
        /// Team ID.
        team_id -> Integer,
        /// Member ID.
        member_id -> Integer,
    }
}

diesel::allow_tables_to_appear_in_same_query!(teams, team_members);

/// A team with optional parent relationship.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = teams)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Team {
    /// Team ID.
    id: i32,
    /// Parent team ID (nullable).
    parent_team_id: Option<i32>,
}

/// A team member association.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = team_members)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct TeamMember {
    /// Team ID.
    team_id: i32,
    /// Member ID.
    member_id: i32,
}

#[test]
fn test_delete_using_in_trigger() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Define the PostgreSQL Schema + Trigger acting as the source
    // relying on internal function parsing simulation of pg2sqlite
    // where body extraction works via schema helper if defined as standard postgres
    // function.
    let sql = "
CREATE TABLE teams (
    id SERIAL PRIMARY KEY,
    parent_team_id INTEGER
);

CREATE TABLE team_members (
    team_id INTEGER,
    member_id INTEGER,
    PRIMARY KEY (team_id, member_id)
);

CREATE FUNCTION propagate_delete() RETURNS TRIGGER AS $$
BEGIN
    DELETE FROM team_members USING teams
    WHERE teams.parent_team_id = OLD.team_id
      AND team_members.team_id = teams.id
      AND team_members.member_id = OLD.member_id;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_remove_team_member_from_child_teams
AFTER DELETE ON team_members
FOR EACH ROW EXECUTE FUNCTION propagate_delete();
";

    // 2. Translate to SQLite
    let translator = Pg2Sqlite::default().sql(sql)?;
    let translated = translator.translate(&Pg2SqliteOptions::default())?;

    // 3. Verify Translation (Insta Snapshot)
    // We convert the vector of statements to a single string for snapshotting
    let translated_sql: String =
        translated.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    insta::assert_snapshot!("delete_using_translation", translated_sql);

    // 4. Runtime Verification (Diesel + SQLite)
    let mut connection = SqliteConnection::establish(":memory:")?;

    // Enable recursive triggers if needed (though this is depth 1)
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut connection)?;

    // Create tables and triggers in SQLite
    for stmt in translated {
        diesel::sql_query(stmt.to_string()).execute(&mut connection)?;
    }

    // A. Setup Data
    // Team 1 (Parent)
    // Team 2 (Child of 1)
    diesel::insert_into(teams::table)
        .values(&Team { id: 1, parent_team_id: None })
        .execute(&mut connection)?;
    diesel::insert_into(teams::table)
        .values(&Team { id: 2, parent_team_id: Some(1) })
        .execute(&mut connection)?;

    // Member 100 in Team 1 and Team 2
    diesel::insert_into(team_members::table)
        .values(&TeamMember { team_id: 1, member_id: 100 })
        .execute(&mut connection)?;
    diesel::insert_into(team_members::table)
        .values(&TeamMember { team_id: 2, member_id: 100 })
        .execute(&mut connection)?;

    // Member 200 in Team 1 only
    diesel::insert_into(team_members::table)
        .values(&TeamMember { team_id: 1, member_id: 200 })
        .execute(&mut connection)?;

    // B. Perform Action (Delete from Parent)
    // Deleting (1, 100) should trigger deletion of (2, 100) via the Translated
    // Trigger
    diesel::delete(team_members::table)
        .filter(team_members::team_id.eq(1))
        .filter(team_members::member_id.eq(100))
        .execute(&mut connection)?;

    // C. Assertions
    // Check (1, 100) is gone (direct action)
    let count: i64 = team_members::table
        .filter(team_members::team_id.eq(1))
        .filter(team_members::member_id.eq(100))
        .count()
        .get_result(&mut connection)?;
    assert_eq!(count, 0, "Member 100 should be deleted from Team 1");

    // Check (2, 100) is gone (TRIGGER action)
    let count: i64 = team_members::table
        .filter(team_members::team_id.eq(2))
        .filter(team_members::member_id.eq(100))
        .count()
        .get_result(&mut connection)?;
    assert_eq!(count, 0, "Member 100 should be deleted from Team 2 (child team) by trigger");

    // Check (1, 200) remains (Safe)
    let count: i64 = team_members::table
        .filter(team_members::team_id.eq(1))
        .filter(team_members::member_id.eq(200))
        .count()
        .get_result(&mut connection)?;
    assert_eq!(count, 1, "Member 200 should remain in Team 1");

    Ok(())
}

/// Test that DELETE...USING correctly references RLS backing tables in the
/// EXISTS subquery.
///
/// **Expected to FAIL initially** - demonstrates the bug where USING tables
/// with RLS are not renamed to their backing tables in the converted EXISTS
/// subquery.
///
/// ## Note on RLS Translation and Diesel ORM
///
/// When pg2sqlite translates a PostgreSQL table with RLS enabled, it creates:
/// 1. A backing table (e.g., `users_rls`) - stores the actual data
/// 2. A view (e.g., `users`) - applies RLS policies
/// 3. INSTEAD OF triggers on the view - handle INSERT/UPDATE/DELETE
///
/// **For real applications using Diesel ORM:**
/// - Only define schemas for the VIEWS (e.g., `users`, `posts`)
/// - DO NOT define schemas for backing tables (e.g., `users_rls`, `posts_rls`)
/// - The backing tables are implementation details and should be transparent
///
/// The backing table schemas in tests are ONLY for verifying the translation
/// implementation works correctly.
#[test]
fn test_delete_using_with_rls_table() -> Result<(), Box<dyn std::error::Error>> {
    use pg2sqlite::traits::TranslationOptions;

    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit".to_string());

    // Combine schema setup and DELETE statement so the translator has full context
    let sql = "
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            username TEXT NOT NULL,
            active BOOLEAN NOT NULL DEFAULT true
        );

        ALTER TABLE users ENABLE ROW LEVEL SECURITY;
        CREATE POLICY users_policy ON users FOR ALL TO PUBLIC USING (true);

        CREATE TABLE posts (
            id SERIAL PRIMARY KEY,
            author_id INTEGER REFERENCES users(id),
            title TEXT NOT NULL,
            content TEXT
        );

        -- This is the statement we're testing
        DELETE FROM posts USING users WHERE posts.author_id = users.id AND users.active = false;
    ";

    let translator = Pg2Sqlite::default().sql(sql)?;
    let translated = translator.translate(&options)?;

    // Find the DELETE statement in the translated output
    let delete_stmt = translated
        .iter()
        .find(|stmt| stmt.to_string().starts_with("DELETE"))
        .expect("Should have DELETE statement");

    let sql_str = delete_stmt.to_string();

    // BUG: Currently the EXISTS subquery references "users" (the view),
    // but it should reference "users_rls" (the backing table).
    // The view filters rows based on RLS policies, which could cause incorrect
    // deletions.

    assert!(
        sql_str.contains("users_rls"),
        "DELETE USING should reference users_rls backing table in EXISTS subquery, got: {sql_str}"
    );

    Ok(())
}
