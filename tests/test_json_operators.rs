//! TDD tests for JSON operator handling (Sections 3 and 4).

#![allow(missing_docs)]

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// Both path operators used to be refused outright. They now become arrow
/// chains; `tests/test_json_path_operators.rs` holds the value assertions and
/// the sharp edges of the path literal.
#[test]
fn test_hash_arrow_translates_to_an_arrow_chain() {
    let translated = Pg2Sqlite::default()
        .sql("CREATE TABLE t (data TEXT); SELECT data #> '{a,b}' FROM t;")
        .and_then(|t| t.translate_to_sql(&Pg2SqliteOptions::default()))
        .expect("#> lowers onto the arrows");
    let query = translated.last().expect("a statement");
    assert_eq!(query, "SELECT data -> 'a' -> 'b' FROM t");
}

#[test]
fn test_hash_arrow_arrow_takes_text_on_the_last_hop() {
    let translated = Pg2Sqlite::default()
        .sql("CREATE TABLE t (data TEXT); SELECT data #>> '{a,b}' FROM t;")
        .and_then(|t| t.translate_to_sql(&Pg2SqliteOptions::default()))
        .expect("#>> lowers onto the arrows");
    let query = translated.last().expect("a statement");
    assert_eq!(query, "SELECT data -> 'a' ->> 'b' FROM t");
}

#[test]
fn test_regular_arrow_still_passes_through() -> Result<(), Box<dyn std::error::Error>> {
    // -> / ->> should still translate without error
    let translated = Pg2Sqlite::default()
        .sql("CREATE TABLE t (data TEXT); SELECT data->'key' FROM t;")
        .and_then(|t| t.translate(&Pg2SqliteOptions::default()))?;
    assert!(!translated.is_empty(), "the document should translate into at least one statement");
    Ok(())
}

#[test]
fn test_json_access_in_trigger_body() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "
        CREATE TABLE events (id SERIAL PRIMARY KEY, payload TEXT);
        CREATE TABLE event_log (id SERIAL PRIMARY KEY, field_value TEXT);

        CREATE FUNCTION log_field() RETURNS TRIGGER AS $$
        BEGIN
            INSERT INTO event_log (field_value) VALUES (NEW.payload->>'type');
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;

        CREATE TRIGGER log_field_trigger
        AFTER INSERT ON events FOR EACH ROW EXECUTE FUNCTION log_field();
    ";

    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }

    // Insert a row — trigger should fire and log the JSON field
    diesel::sql_query(r#"INSERT INTO events (id, payload) VALUES (1, '{"type":"click"}')"#)
        .execute(&mut conn)?;

    #[derive(diesel::QueryableByName)]
    struct LogRow {
        #[diesel(sql_type = diesel::sql_types::Text)]
        field_value: String,
    }

    let rows = diesel::sql_query("SELECT field_value FROM event_log").load::<LogRow>(&mut conn)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].field_value, "click");
    Ok(())
}
