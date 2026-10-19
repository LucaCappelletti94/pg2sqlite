//! Reconnect preserves temporary state and transaction outcomes.

use diesel::{
    prelude::*,
    result::{DatabaseErrorKind, Error as DatabaseError},
};

use super::{open, run, script};

diesel::table! {
    scratch (id) { id -> Integer, value -> Integer, }
}
diesel::table! {
    mirror (id) { id -> Integer, value -> Integer, }
}
diesel::table! {
    docs (id) { id -> Integer, title -> Text, }
}
diesel::table! {
    selected (id) { id -> Integer, }
}
diesel::table! {
    docs_fts (rowid) { rowid -> BigInt, body -> Text, }
}
diesel::infix_operator!(Matches, " MATCH ", backend: diesel::sqlite::Sqlite);

fn reopened(source: &str) -> (tempfile::TempDir, SqliteConnection) {
    let script = script(source);
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    (files, reopened)
}

fn rows(connection: &mut SqliteConnection) -> Vec<(i32, i32)> {
    scratch::table.order(scratch::id).load(connection).expect("temporary rows")
}

#[test]
fn update_from_and_aliased_delete_replay_temporary_rows() {
    let (_files, mut connection) = reopened(
        "CREATE TABLE rates(id INT PRIMARY KEY, amount INT NOT NULL);
         INSERT INTO rates VALUES(1,10);
         CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         INSERT INTO scratch VALUES(1,1),(2,2),(3,3);
         UPDATE scratch AS s SET value=s.value+r.amount FROM rates AS r WHERE s.id=r.id;
         DELETE FROM scratch AS s WHERE s.id=2;",
    );
    assert_eq!(rows(&mut connection), [(1, 11), (3, 3)]);
}

#[test]
fn tuple_assignment_replays_temporary_rows() {
    let (_files, mut connection) = reopened(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         INSERT INTO scratch VALUES(1,2);
         UPDATE scratch SET (id,value)=(id+10,value+20);",
    );
    assert_eq!(rows(&mut connection), [(11, 22)]);
}

#[test]
fn partial_expression_index_replays_its_uniqueness_boundary() {
    let (_files, mut connection) = reopened(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE UNIQUE INDEX positive_value ON scratch((abs(value))) WHERE value>0;
         INSERT INTO scratch VALUES(1,4),(2,-4);",
    );
    let duplicate = diesel::insert_into(scratch::table)
        .values((scratch::id.eq(3), scratch::value.eq(4)))
        .execute(&mut connection);
    assert!(matches!(
        duplicate,
        Err(DatabaseError::DatabaseError(DatabaseErrorKind::UniqueViolation, _))
    ));
    diesel::insert_into(scratch::table)
        .values((scratch::id.eq(4), scratch::value.eq(-4)))
        .execute(&mut connection)
        .expect("the excluded row remains admissible");
    assert_eq!(rows(&mut connection), [(1, 4), (2, -4), (4, -4)]);
}

#[test]
fn replayed_temporary_trigger_updates_and_deletes_its_dependencies() {
    let (_files, mut connection) = reopened(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE TEMP TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE FUNCTION synchronize() RETURNS trigger AS $$
         BEGIN
             UPDATE mirror SET value=NEW.value WHERE id=NEW.id;
             DELETE FROM mirror WHERE value<0;
             RETURN NEW;
         END; $$ LANGUAGE plpgsql;
         CREATE TRIGGER synchronize AFTER INSERT ON scratch FOR EACH ROW EXECUTE FUNCTION synchronize();
         INSERT INTO mirror VALUES(1,0),(2,-2);
         INSERT INTO scratch VALUES(1,7);",
    );
    let initial = mirror::table
        .order(mirror::id)
        .load::<(i32, i32)>(&mut connection)
        .expect("trigger seed effects");
    assert_eq!(initial, [(1, 7)]);
    diesel::insert_into(mirror::table)
        .values((mirror::id.eq(3), mirror::value.eq(0)))
        .execute(&mut connection)
        .expect("new mirror row");
    diesel::insert_into(scratch::table)
        .values((scratch::id.eq(3), scratch::value.eq(9)))
        .execute(&mut connection)
        .expect("application insert");
    let after = mirror::table
        .order(mirror::id)
        .load::<(i32, i32)>(&mut connection)
        .expect("live trigger effects");
    assert_eq!(after, [(1, 7), (3, 9)]);
}

#[test]
fn dropped_trigger_owner_keeps_effects_needed_by_a_surviving_table() {
    let script = script(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE TEMP TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE INDEX scratch_value ON scratch(value);
         CREATE FUNCTION remember() RETURNS trigger AS $$
         BEGIN INSERT INTO mirror VALUES(NEW.id,NEW.value); RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER remember AFTER INSERT ON scratch FOR EACH ROW EXECUTE FUNCTION remember();
         INSERT INTO scratch VALUES(1,40);
         DROP TABLE scratch;
         CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         INSERT INTO scratch VALUES(2,50);",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
        let initialized = mirror::table
            .order(mirror::id)
            .load::<(i32, i32)>(&mut creating)
            .expect("initialized trigger output");
        assert_eq!(initialized, [(1, 40)]);
    }
    let mut connection = open(&files);
    run(&mut connection, script.connect().expect("connection program"));
    assert_eq!(rows(&mut connection), [(2, 50)]);
    let remembered = mirror::table
        .order(mirror::id)
        .load::<(i32, i32)>(&mut connection)
        .expect("historical trigger output");
    assert_eq!(remembered, [(1, 40)]);
}

#[test]
fn savepoint_rollback_and_separate_commits_preserve_only_retained_writes() {
    let (_files, mut connection) = reopened(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         BEGIN;
         INSERT INTO scratch VALUES(1,1);
         SAVEPOINT discard;
         UPDATE scratch SET value=100 WHERE id=1;
         INSERT INTO scratch VALUES(2,2);
         ROLLBACK TO SAVEPOINT discard;
         RELEASE SAVEPOINT discard;
         INSERT INTO scratch VALUES(3,3);
         COMMIT;
         BEGIN;
         INSERT INTO scratch VALUES(4,4);
         COMMIT;",
    );
    assert_eq!(rows(&mut connection), [(1, 1), (3, 3), (4, 4)]);
}

#[test]
fn full_text_storage_and_maintenance_survive_connection_replay() {
    use diesel::expression::IntoSql;
    let (_files, mut connection) = reopened(
        "CREATE TABLE docs(id INT PRIMARY KEY, body TEXT NOT NULL);
         CREATE INDEX docs_fts ON docs USING GIN(to_tsvector('english',body));
         INSERT INTO docs VALUES(1,'needle one');
         CREATE TEMP VIEW selected AS SELECT id FROM docs WHERE body LIKE 'needle%';
         ALTER TABLE docs RENAME COLUMN body TO title;",
    );
    let selected = selected::table
        .select(selected::id)
        .load::<i32>(&mut connection)
        .expect("deferred predicate");
    assert_eq!(selected, [1]);
    diesel::insert_into(docs::table)
        .values((docs::id.eq(2), docs::title.eq("needle two")))
        .execute(&mut connection)
        .expect("persistent maintenance");
    let indexed = docs_fts::table
        .filter(Matches::new(docs_fts::body, "needle".into_sql::<diesel::sql_types::Text>()))
        .select(docs_fts::rowid)
        .order(docs_fts::rowid)
        .load::<i64>(&mut connection)
        .expect("indexed rows");
    assert_eq!(indexed, [1, 2]);
}

#[test]
fn transitive_trigger_effects_outlive_both_intermediate_owners() {
    let (_files, mut connection) = reopened(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE TEMP TABLE middle(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE TEMP TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE FUNCTION first_hop() RETURNS trigger AS $$
         BEGIN INSERT INTO middle VALUES(NEW.id,NEW.value); RETURN NEW; END; $$ LANGUAGE plpgsql;
         CREATE FUNCTION second_hop() RETURNS trigger AS $$
         BEGIN INSERT INTO mirror VALUES(NEW.id,NEW.value); RETURN NEW; END; $$ LANGUAGE plpgsql;
         CREATE TRIGGER first_hop AFTER INSERT ON scratch FOR EACH ROW EXECUTE FUNCTION first_hop();
         CREATE TRIGGER second_hop AFTER INSERT ON middle FOR EACH ROW EXECUTE FUNCTION second_hop();
         INSERT INTO scratch VALUES(1,40);
         DROP TABLE scratch;
         DROP TABLE middle;",
    );
    let values = mirror::table.load::<(i32, i32)>(&mut connection).expect("transitive effects");
    assert_eq!(values, [(1, 40)]);
}

#[test]
fn every_trigger_event_retains_historical_temporary_effects() {
    for (event, row, write, expected) in [
        ("INSERT", "NEW", "INSERT INTO scratch VALUES(2,20)", (2, 20)),
        ("UPDATE", "NEW", "UPDATE scratch SET value=30 WHERE id=1", (1, 30)),
        ("DELETE", "OLD", "DELETE FROM scratch WHERE id=1", (1, 10)),
    ] {
        let source = format!(
            "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
             CREATE TEMP TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
             INSERT INTO scratch VALUES(1,10);
             CREATE FUNCTION remember() RETURNS trigger AS $$
             BEGIN INSERT INTO mirror VALUES({row}.id,{row}.value); RETURN {row}; END; $$ LANGUAGE plpgsql;
             CREATE TRIGGER remember AFTER {event} ON scratch FOR EACH ROW EXECUTE FUNCTION remember();
             {write};
             DROP TABLE scratch;"
        );
        let (_files, mut connection) = reopened(&source);
        let values = mirror::table.load::<(i32, i32)>(&mut connection).expect("event effects");
        assert_eq!(values, [expected], "{event}");
    }
}

#[test]
fn reconnect_refuses_to_repeat_persistent_trigger_effects() {
    let script = script(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE FUNCTION remember() RETURNS trigger AS $$
         BEGIN INSERT INTO mirror VALUES(NEW.id,NEW.value); RETURN NEW; END; $$ LANGUAGE plpgsql;
         CREATE TRIGGER remember AFTER INSERT ON scratch FOR EACH ROW EXECUTE FUNCTION remember();
         INSERT INTO scratch VALUES(1,40);",
    );
    let files = tempfile::TempDir::new().expect("files");
    let mut connection = open(&files);
    run(&mut connection, script.iter());
    assert_eq!(
        mirror::table.load::<(i32, i32)>(&mut connection).expect("persistent initialization"),
        [(1, 40)]
    );
    assert!(matches!(
        script.connect(),
        Err(pg2sqlite::prelude::Error::Connection(
            pg2sqlite::script::ConnectionError::Translation(_)
        ))
    ));
}

#[test]
fn reconnect_refuses_persistent_writes_needed_by_a_temporary_trigger() {
    let script = script(
        "CREATE TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE TEMP TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE FUNCTION remember() RETURNS trigger AS $$
         BEGIN INSERT INTO mirror VALUES(NEW.id,NEW.value); RETURN NEW; END; $$ LANGUAGE plpgsql;
         CREATE TEMP TRIGGER remember AFTER INSERT ON scratch FOR EACH ROW EXECUTE FUNCTION remember();
         INSERT INTO scratch VALUES(1,40);",
    );
    let files = tempfile::TempDir::new().expect("files");
    let mut connection = open(&files);
    run(&mut connection, script.iter());
    assert_eq!(
        mirror::table.load::<(i32, i32)>(&mut connection).expect("temporary initialization"),
        [(1, 40)]
    );
    assert!(matches!(
        script.connect(),
        Err(pg2sqlite::prelude::Error::Connection(
            pg2sqlite::script::ConnectionError::Translation(_)
        ))
    ));
}

#[test]
fn trigger_reads_retain_writes_between_definition_and_invocation() {
    let (_files, mut connection) = reopened(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE TEMP TABLE rates(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE TEMP TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE FUNCTION remember() RETURNS trigger AS $$
         BEGIN
             INSERT INTO mirror SELECT NEW.id,r.value FROM rates AS r WHERE r.id=NEW.id;
             RETURN NEW;
         END; $$ LANGUAGE plpgsql;
         CREATE TRIGGER remember AFTER INSERT ON scratch FOR EACH ROW EXECUTE FUNCTION remember();
         INSERT INTO rates VALUES(1,42);
         INSERT INTO scratch VALUES(1,0);
         DROP TABLE scratch;
         DROP TABLE rates;",
    );
    let values = mirror::table.load::<(i32, i32)>(&mut connection).expect("trigger read effects");
    assert_eq!(values, [(1, 42)]);
}

#[test]
fn cyclic_potential_trigger_effects_terminate_without_losing_rows() {
    let (_files, mut connection) = reopened(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE TEMP TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE FUNCTION copy_row() RETURNS trigger AS $$
         BEGIN INSERT INTO mirror VALUES(NEW.id,NEW.value); RETURN NEW; END; $$ LANGUAGE plpgsql;
         CREATE FUNCTION update_source() RETURNS trigger AS $$
         BEGIN UPDATE scratch SET value=NEW.value WHERE id=NEW.id; RETURN NEW; END; $$ LANGUAGE plpgsql;
         CREATE TRIGGER copy_insert AFTER INSERT ON scratch FOR EACH ROW EXECUTE FUNCTION copy_row();
         CREATE TRIGGER copy_update AFTER UPDATE ON scratch FOR EACH ROW WHEN (NEW.value<0) EXECUTE FUNCTION copy_row();
         CREATE TRIGGER update_source AFTER INSERT ON mirror FOR EACH ROW EXECUTE FUNCTION update_source();
         INSERT INTO scratch VALUES(1,7);
         DROP TABLE scratch;",
    );
    let values = mirror::table.load::<(i32, i32)>(&mut connection).expect("cyclic effects");
    assert_eq!(values, [(1, 7)]);
}

#[test]
fn dropped_and_rolled_back_triggers_do_not_add_persistent_effects() {
    for removal in [
        "DROP TRIGGER remember ON scratch;",
        "ROLLBACK;",
        "BEGIN; DROP TRIGGER remember ON scratch; ROLLBACK; DROP TRIGGER remember ON scratch;",
    ] {
        let begin = if removal == "ROLLBACK;" { "BEGIN;" } else { "" };
        let source = format!(
            "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
             CREATE TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
             CREATE FUNCTION remember() RETURNS trigger AS $$
             BEGIN INSERT INTO mirror VALUES(NEW.id,NEW.value); RETURN NEW; END; $$ LANGUAGE plpgsql;
             {begin}
             CREATE TRIGGER remember AFTER INSERT ON scratch FOR EACH ROW EXECUTE FUNCTION remember();
             {removal}
             INSERT INTO scratch VALUES(1,7);"
        );
        let (_files, mut connection) = reopened(&source);
        assert_eq!(rows(&mut connection), [(1, 7)], "{removal}");
        assert_eq!(mirror::table.load::<(i32, i32)>(&mut connection).expect("persistent rows"), []);
    }
}

#[test]
fn unrelated_trigger_events_do_not_refuse_temporary_writes() {
    let (_files, mut connection) = reopened(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE FUNCTION remember() RETURNS trigger AS $$
         BEGIN INSERT INTO mirror VALUES(NEW.id,NEW.value); RETURN NEW; END; $$ LANGUAGE plpgsql;
         CREATE TRIGGER remember AFTER UPDATE ON scratch FOR EACH ROW EXECUTE FUNCTION remember();
         INSERT INTO scratch VALUES(1,7);",
    );
    assert_eq!(rows(&mut connection), [(1, 7)]);
    assert_eq!(mirror::table.load::<(i32, i32)>(&mut connection).expect("persistent rows"), []);
}

#[test]
fn application_writes_can_use_replayed_triggers_targeting_renamed_persistent_tables() {
    diesel::table! {
        archive (id) { id -> Integer, value -> Integer, }
    }
    let (_files, mut connection) = reopened(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE FUNCTION remember() RETURNS trigger AS $$
         BEGIN INSERT INTO mirror(id,value) VALUES(NEW.id,NEW.value); RETURN NEW; END; $$ LANGUAGE plpgsql;
         CREATE TRIGGER remember AFTER INSERT ON scratch FOR EACH ROW EXECUTE FUNCTION remember();
         ALTER TABLE mirror RENAME TO archive;",
    );
    diesel::insert_into(scratch::table)
        .values((scratch::id.eq(1), scratch::value.eq(42)))
        .execute(&mut connection)
        .expect("application trigger write");
    assert_eq!(
        archive::table.load::<(i32, i32)>(&mut connection).expect("renamed persistent target"),
        [(1, 42)]
    );
}

#[test]
fn conflict_updates_include_update_trigger_effects_in_reconnect_safety() {
    let script = script(
        "CREATE TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         INSERT INTO scratch VALUES(1,0);
         CREATE TEMP TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE FUNCTION remember() RETURNS trigger AS $$
         BEGIN INSERT INTO mirror VALUES(NEW.id,NEW.value); RETURN NEW; END; $$ LANGUAGE plpgsql;
         CREATE TEMP TRIGGER remember AFTER UPDATE ON scratch FOR EACH ROW EXECUTE FUNCTION remember();
         INSERT INTO scratch VALUES(1,40) ON CONFLICT(id) DO UPDATE SET value=excluded.value;",
    );
    let files = tempfile::TempDir::new().expect("files");
    let mut connection = open(&files);
    run(&mut connection, script.iter());
    assert_eq!(
        mirror::table.load::<(i32, i32)>(&mut connection).expect("conflict update effect"),
        [(1, 40)]
    );
    assert!(matches!(
        script.connect(),
        Err(pg2sqlite::prelude::Error::Connection(
            pg2sqlite::script::ConnectionError::Translation(_)
        ))
    ));
}

#[test]
fn rolled_back_trigger_drop_preserves_reconnect_safety() {
    let script = script(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE FUNCTION remember() RETURNS trigger AS $$
         BEGIN INSERT INTO mirror VALUES(NEW.id,NEW.value); RETURN NEW; END; $$ LANGUAGE plpgsql;
         CREATE TRIGGER remember AFTER INSERT ON scratch FOR EACH ROW EXECUTE FUNCTION remember();
         BEGIN;
         DROP TRIGGER remember ON scratch;
         ROLLBACK;
         INSERT INTO scratch VALUES(1,40);",
    );
    let files = tempfile::TempDir::new().expect("files");
    let mut connection = open(&files);
    run(&mut connection, script.iter());
    assert_eq!(
        mirror::table.load::<(i32, i32)>(&mut connection).expect("restored trigger effect"),
        [(1, 40)]
    );
    assert!(matches!(
        script.connect(),
        Err(pg2sqlite::prelude::Error::Connection(
            pg2sqlite::script::ConnectionError::Translation(_)
        ))
    ));
}

#[test]
fn unused_mixed_history_neither_refuses_nor_repeats_persistent_writes() {
    let (_files, mut connection) = reopened(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE FUNCTION remember() RETURNS trigger AS $$
         BEGIN INSERT INTO mirror VALUES(NEW.id,NEW.value); RETURN NEW; END; $$ LANGUAGE plpgsql;
         CREATE TRIGGER remember AFTER INSERT ON scratch FOR EACH ROW EXECUTE FUNCTION remember();
         INSERT INTO scratch VALUES(1,40);
         DROP TABLE scratch;
         CREATE TEMP VIEW selected AS SELECT id FROM mirror;",
    );
    assert_eq!(
        selected::table.select(selected::id).load::<i32>(&mut connection).expect("temporary view"),
        [1]
    );
    assert_eq!(
        mirror::table.load::<(i32, i32)>(&mut connection).expect("persistent trigger effects"),
        [(1, 40)]
    );
}

#[test]
fn unresolved_trigger_destination_keeps_fresh_initialization_usable() {
    let script = script(
        "CREATE TEMP TABLE scratch(id INT PRIMARY KEY, value INT NOT NULL);
         CREATE FUNCTION remember() RETURNS trigger AS $$
         BEGIN INSERT INTO mirror VALUES(NEW.id,NEW.value); RETURN NEW; END; $$ LANGUAGE plpgsql;
         CREATE TRIGGER remember AFTER INSERT ON scratch FOR EACH ROW EXECUTE FUNCTION remember();
         CREATE TEMP TABLE mirror(id INT PRIMARY KEY, value INT NOT NULL);
         INSERT INTO scratch VALUES(1,40);",
    );
    let files = tempfile::TempDir::new().expect("files");
    let mut connection = open(&files);
    run(&mut connection, script.iter());
    assert_eq!(
        mirror::table.load::<(i32, i32)>(&mut connection).expect("late destination initialization"),
        [(1, 40)]
    );
    assert!(matches!(
        script.connect(),
        Err(pg2sqlite::prelude::Error::Connection(
            pg2sqlite::script::ConnectionError::Translation(_)
        ))
    ));
}
