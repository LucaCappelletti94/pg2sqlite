#![allow(missing_docs)]
//! Finding 1: maintenance trigger must fire when the user updates the
//! maintenance column itself and must not recurse.
//! Finding 6: multiple non-maintenance BEFORE/AFTER triggers with the same
//! event/timing are refused (PostgreSQL: name order; SQLite: reverse creation
//! order — no faithful emission exists).

use diesel::{Connection, RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

diesel::table! {
    mut_tbl (id) {
        id -> Integer,
        tag -> Nullable<Text>,
    }
}

const BUMP_TAG_SCHEMA: &str = "
    CREATE TABLE mut (id INT PRIMARY KEY, tag TEXT);
    CREATE FUNCTION bump_tag() RETURNS trigger AS $$
    BEGIN
        NEW.tag := NEW.tag || '!';
        RETURN NEW;
    END;
    $$ LANGUAGE plpgsql;
    CREATE TRIGGER mut_bump BEFORE UPDATE ON mut FOR EACH ROW EXECUTE FUNCTION bump_tag();
";

const FIRING_ORDER_SCHEMA: &str = "
    CREATE TABLE trg3 (id INT PRIMARY KEY);
    CREATE TABLE trg3_log (seq SERIAL PRIMARY KEY, marker TEXT);
    CREATE FUNCTION trg3_b() RETURNS trigger AS $$
    BEGIN INSERT INTO trg3_log (marker) VALUES ('b'); RETURN NEW; END;
    $$ LANGUAGE plpgsql;
    CREATE FUNCTION trg3_a() RETURNS trigger AS $$
    BEGIN INSERT INTO trg3_log (marker) VALUES ('a'); RETURN NEW; END;
    $$ LANGUAGE plpgsql;
    CREATE FUNCTION trg3_m() RETURNS trigger AS $$
    BEGIN INSERT INTO trg3_log (marker) VALUES ('m'); RETURN NEW; END;
    $$ LANGUAGE plpgsql;
    CREATE TRIGGER zz3_b BEFORE INSERT ON trg3 FOR EACH ROW EXECUTE FUNCTION trg3_b();
    CREATE TRIGGER aa3_a BEFORE INSERT ON trg3 FOR EACH ROW EXECUTE FUNCTION trg3_a();
    CREATE TRIGGER mm3_m BEFORE INSERT ON trg3 FOR EACH ROW EXECUTE FUNCTION trg3_m();
";

fn apply_schema(pg: &str) -> SqliteConnection {
    let stmts = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate");
    let mut conn = SqliteConnection::establish(":memory:").expect("connect");
    // Translated DDL cannot be expressed via diesel's typed DSL.
    diesel::sql_query("PRAGMA recursive_triggers = ON").execute(&mut conn).expect("pragma");
    for stmt in &stmts {
        diesel::sql_query(stmt.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("DDL: {e}\n{stmt}"));
    }
    conn
}

#[derive(QueryableByName, Debug)]
struct TagRow {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    tag: Option<String>,
}

fn read_tag(conn: &mut SqliteConnection) -> Option<String> {
    diesel::sql_query("SELECT tag FROM mut WHERE id = 1")
        .get_result::<TagRow>(conn)
        .expect("select")
        .tag
}

/// PostgreSQL ground truth: UPDATE SET tag='y' → tag='y!' (trigger fires).
#[test]
fn maintenance_trigger_fires_on_update_to_maintenance_column() {
    let mut conn = apply_schema(BUMP_TAG_SCHEMA);
    diesel::sql_query("INSERT INTO mut (id, tag) VALUES (1, 'x')")
        .execute(&mut conn)
        .expect("insert");
    diesel::sql_query("UPDATE mut SET tag = 'y' WHERE id = 1").execute(&mut conn).expect("update");
    assert_eq!(read_tag(&mut conn).as_deref(), Some("y!"));
}

/// Two chained updates: PostgreSQL gives 'z!' (each fires once).
#[test]
fn maintenance_trigger_chains_on_second_update() {
    let mut conn = apply_schema(BUMP_TAG_SCHEMA);
    diesel::sql_query("INSERT INTO mut (id, tag) VALUES (1, 'x')")
        .execute(&mut conn)
        .expect("insert");
    diesel::sql_query("UPDATE mut SET tag = 'y' WHERE id = 1").execute(&mut conn).expect("first");
    diesel::sql_query("UPDATE mut SET tag = 'z' WHERE id = 1").execute(&mut conn).expect("second");
    assert_eq!(read_tag(&mut conn).as_deref(), Some("z!"));
}

/// WHEN clause must prevent the trigger from re-firing on its own UPDATE.
/// Recursion would produce 'y!!' instead of 'y!'.
#[test]
fn maintenance_trigger_does_not_recurse_with_recursive_triggers_enabled() {
    let mut conn = apply_schema(BUMP_TAG_SCHEMA);
    diesel::sql_query("INSERT INTO mut (id, tag) VALUES (1, 'x')")
        .execute(&mut conn)
        .expect("insert");
    diesel::sql_query("UPDATE mut SET tag = 'y' WHERE id = 1").execute(&mut conn).expect("update");
    assert_eq!(read_tag(&mut conn).as_deref(), Some("y!"));
}

#[test]
fn single_trigger_per_event_still_translates() {
    Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY);
             CREATE FUNCTION f() RETURNS trigger AS $$ BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql;
             CREATE TRIGGER t_ai BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();",
        )
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("single trigger translates");
}

/// Three BEFORE INSERT triggers on one table: PostgreSQL fires in name order,
/// SQLite in reverse creation order — refuse because no faithful form exists.
#[test]
fn multiple_standard_triggers_same_event_timing_refused() {
    let err = Pg2Sqlite::default()
        .sql(FIRING_ORDER_SCHEMA)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("three BEFORE INSERT triggers must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("creation order")
            || msg.contains("name order")
            || msg.contains("firing order"),
        "refusal must name the ordering divergence; got: {msg}"
    );
}
