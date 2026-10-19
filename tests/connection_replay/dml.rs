//! Replayed writes preserve their physical effects and column bindings.

use diesel::prelude::*;

use super::{open, run, script};

diesel::table! { scratch (id) { id -> Integer, } }
diesel::table! { source (id) { id -> Integer, } }
diesel::table! { ledger (id) { id -> Integer, amount -> Integer, } }
diesel::table! { inputs (id) { id -> Integer, } }
diesel::table! {
    #[sql_name = "ledger"]
    changed_key (key) { key -> Integer, amount -> Integer, }
}

#[test]
fn a_view_read_does_not_become_a_persistent_write() {
    let script = script(
        "CREATE TABLE source(id INT PRIMARY KEY);
         INSERT INTO source VALUES(1);
         CREATE TEMP TABLE scratch(id INT PRIMARY KEY);
         CREATE TEMP VIEW chosen AS SELECT scratch.id FROM scratch
             WHERE EXISTS(SELECT 1 FROM source);
         CREATE FUNCTION put_row() RETURNS trigger AS $$
             BEGIN INSERT INTO scratch VALUES(NEW.id); RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER put_row INSTEAD OF INSERT ON chosen
             FOR EACH ROW EXECUTE FUNCTION put_row();
         INSERT INTO chosen VALUES(2);",
    );
    let files = tempfile::tempdir().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
        assert_eq!(
            scratch::table.select(scratch::id).load::<i32>(&mut creating).expect("initial scratch"),
            [2]
        );
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("temporary-only write"));
    assert_eq!(
        scratch::table.select(scratch::id).load::<i32>(&mut reopened).expect("replayed scratch"),
        [2]
    );
    assert_eq!(
        source::table.select(source::id).load::<i32>(&mut reopened).expect("persistent source"),
        [1]
    );
}

#[test]
fn an_upsert_trigger_uses_renamed_assignment_and_excluded_columns() {
    let script = script(
        "CREATE TABLE ledger(id INT PRIMARY KEY, value INT NOT NULL);
         INSERT INTO ledger VALUES(1,10);
         CREATE TEMP TABLE inputs(id INT);
         CREATE FUNCTION apply_input() RETURNS trigger AS $$
             BEGIN
                 INSERT INTO ledger(id,value) VALUES(NEW.id,NEW.id)
                     ON CONFLICT(id) DO UPDATE SET value=EXCLUDED.value;
                 RETURN NEW;
             END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER apply_input AFTER INSERT ON inputs
             FOR EACH ROW EXECUTE FUNCTION apply_input();
         ALTER TABLE ledger RENAME COLUMN value TO amount;",
    );
    let files = tempfile::tempdir().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
        diesel::insert_into(inputs::table)
            .values(inputs::id.eq(1))
            .execute(&mut creating)
            .expect("initial upsert");
        assert_eq!(
            ledger::table.load::<(i32, i32)>(&mut creating).expect("initial ledger"),
            [(1, 1)]
        );
        diesel::update(ledger::table)
            .set(ledger::amount.eq(90))
            .execute(&mut creating)
            .expect("application update");
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    assert_eq!(
        ledger::table.load::<(i32, i32)>(&mut reopened).expect("persistent ledger"),
        [(1, 90)]
    );
    diesel::insert_into(inputs::table)
        .values(inputs::id.eq(1))
        .execute(&mut reopened)
        .expect("reconnected upsert");
    assert_eq!(ledger::table.load::<(i32, i32)>(&mut reopened).expect("updated ledger"), [(1, 1)]);
}

#[test]
fn conflict_targets_and_predicates_keep_their_column_bindings() {
    let script = script(
        "CREATE TABLE ledger(id INT PRIMARY KEY, value INT NOT NULL);
         INSERT INTO ledger VALUES(1,5);
         CREATE TEMP TABLE inputs(id INT);
         CREATE FUNCTION apply_input() RETURNS trigger AS $$
             BEGIN
                 INSERT INTO ledger(id,value) VALUES(1,NEW.id)
                     ON CONFLICT(id) DO UPDATE SET value=EXCLUDED.value
                     WHERE EXCLUDED.value > ledger.value;
                 RETURN NEW;
             END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER apply_input AFTER INSERT ON inputs
             FOR EACH ROW EXECUTE FUNCTION apply_input();
         ALTER TABLE ledger RENAME COLUMN id TO key;
         ALTER TABLE ledger RENAME COLUMN value TO amount;",
    );
    let files = tempfile::tempdir().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
        diesel::insert_into(inputs::table)
            .values(inputs::id.eq(7))
            .execute(&mut creating)
            .expect("initial accepted update");
        diesel::insert_into(inputs::table)
            .values(inputs::id.eq(3))
            .execute(&mut creating)
            .expect("initial rejected update");
        assert_eq!(
            changed_key::table.load::<(i32, i32)>(&mut creating).expect("initial predicate"),
            [(1, 7)]
        );
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    diesel::insert_into(inputs::table)
        .values(inputs::id.eq(9))
        .execute(&mut reopened)
        .expect("reconnected accepted update");
    diesel::insert_into(inputs::table)
        .values(inputs::id.eq(8))
        .execute(&mut reopened)
        .expect("reconnected rejected update");
    assert_eq!(
        changed_key::table.load::<(i32, i32)>(&mut reopened).expect("reconnected predicate"),
        [(1, 9)]
    );
}
