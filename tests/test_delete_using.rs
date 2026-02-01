//! Test for DELETE ... USING translation inside triggers.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[derive(diesel::QueryableByName)]
struct Count {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    count: i64,
}

#[test]
fn test_delete_using_in_trigger() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Define the PostgreSQL Schema + Trigger acting as the source
    // relying on internal function parsing simulation of pg2sqlite
    // where body extraction works via schema helper if defined as standard postgres
    // function.
    let sql = r"
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
    diesel::sql_query("INSERT INTO teams (id, parent_team_id) VALUES (1, NULL)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO teams (id, parent_team_id) VALUES (2, 1)")
        .execute(&mut connection)?;

    // Member 100 in Team 1 and Team 2
    diesel::sql_query("INSERT INTO team_members (team_id, member_id) VALUES (1, 100)")
        .execute(&mut connection)?;
    diesel::sql_query("INSERT INTO team_members (team_id, member_id) VALUES (2, 100)")
        .execute(&mut connection)?;

    // Member 200 in Team 1 only
    diesel::sql_query("INSERT INTO team_members (team_id, member_id) VALUES (1, 200)")
        .execute(&mut connection)?;

    // B. Perform Action (Delete from Parent)
    // Deleting (1, 100) should trigger deletion of (2, 100) via the Translated
    // Trigger
    diesel::sql_query("DELETE FROM team_members WHERE team_id = 1 AND member_id = 100")
        .execute(&mut connection)?;

    // C. Assertions
    // Define a helper struct

    // Check (1, 100) is gone (direct action)
    let result: Vec<Count> = diesel::sql_query(
        "SELECT COUNT(*) as count FROM team_members WHERE team_id = 1 AND member_id = 100",
    )
    .load(&mut connection)?;
    assert_eq!(result[0].count, 0, "Member 100 should be deleted from Team 1");

    // Check (2, 100) is gone (TRIGGER action)
    let result: Vec<Count> = diesel::sql_query(
        "SELECT COUNT(*) as count FROM team_members WHERE team_id = 2 AND member_id = 100",
    )
    .load(&mut connection)?;
    assert_eq!(
        result[0].count, 0,
        "Member 100 should be deleted from Team 2 (child team) by trigger"
    );

    // Check (1, 200) remains (Safe)
    let result: Vec<Count> = diesel::sql_query(
        "SELECT COUNT(*) as count FROM team_members WHERE team_id = 1 AND member_id = 200",
    )
    .load(&mut connection)?;
    assert_eq!(result[0].count, 1, "Member 200 should remain in Team 1");

    Ok(())
}
