//! Transaction outcomes preserve connection-local objects and rows.

use diesel::prelude::*;

use super::{open, run, script};

diesel::table! { t (id) { id -> Integer, } }
diesel::table! { u (id) { id -> Integer, } }
diesel::table! { view_state (id) { id -> Integer, } }

fn assert_replayed_rows(source: &str, expected: &[i32]) {
    let script = script(source);
    let files = tempfile::tempdir().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
        let rows =
            t::table.select(t::id).order(t::id).load::<i32>(&mut creating).expect("initial rows");
        assert_eq!(rows, expected);
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let rows =
        t::table.select(t::id).order(t::id).load::<i32>(&mut reopened).expect("replayed rows");
    assert_eq!(rows, expected);
}

#[test]
fn rolled_back_drop_restores_the_target_of_later_writes() {
    assert_replayed_rows(
        "CREATE TEMP TABLE t(id INT);
         BEGIN;
         DROP TABLE t;
         ROLLBACK;
         INSERT INTO t VALUES(1);",
        &[1],
    );
}

#[test]
fn rolled_back_drop_preserves_a_surviving_empty_table() {
    let script = script(
        "CREATE TEMP TABLE t(id INT);
         CREATE TEMP TABLE u(id INT);
         BEGIN;
         DROP TABLE t;
         ROLLBACK;
         INSERT INTO u VALUES(1);",
    );
    let files = tempfile::tempdir().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
        assert_eq!(t::table.select(t::id).load::<i32>(&mut creating).expect("initial t"), []);
        assert_eq!(u::table.select(u::id).load::<i32>(&mut creating).expect("initial u"), [1]);
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    assert_eq!(t::table.select(t::id).load::<i32>(&mut reopened).expect("surviving t"), []);
    assert_eq!(u::table.select(u::id).load::<i32>(&mut reopened).expect("replayed u"), [1]);
}

#[test]
fn releasing_a_shadowed_savepoint_preserves_the_outer_binding() {
    assert_replayed_rows(
        "CREATE TEMP TABLE t(id INT);
         BEGIN;
         SAVEPOINT seed;
         INSERT INTO t VALUES(1);
         SAVEPOINT seed;
         INSERT INTO t VALUES(2);
         RELEASE SAVEPOINT seed;
         ROLLBACK TO SAVEPOINT seed;
         INSERT INTO t VALUES(3);
         COMMIT;",
        &[3],
    );
}

#[test]
fn a_rollback_target_remains_available_for_another_rollback() {
    assert_replayed_rows(
        "CREATE TEMP TABLE t(id INT);
         BEGIN;
         SAVEPOINT seed;
         INSERT INTO t VALUES(1);
         SAVEPOINT child;
         INSERT INTO t VALUES(2);
         ROLLBACK TO SAVEPOINT seed;
         INSERT INTO t VALUES(3);
         ROLLBACK TO SAVEPOINT seed;
         INSERT INTO t VALUES(4);
         RELEASE SAVEPOINT seed;
         COMMIT;",
        &[4],
    );
}

#[test]
fn released_catalog_changes_remain_owned_by_the_transaction() {
    assert_replayed_rows(
        "CREATE TEMP TABLE t(id INT);
         BEGIN;
         SAVEPOINT seed;
         DROP TABLE t;
         RELEASE SAVEPOINT seed;
         ROLLBACK;
         INSERT INTO t VALUES(40);",
        &[40],
    );
}

#[test]
fn disconnect_discards_uncommitted_catalog_changes() {
    let script = script("CREATE TEMP TABLE t(id INT); BEGIN; DROP TABLE t;");
    let files = tempfile::tempdir().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("committed catalog"));
    assert_eq!(t::table.select(t::id).load::<i32>(&mut reopened).expect("surviving table"), []);
}

fn assert_replayed_view(source: &str) {
    let script = script(source);
    let files = tempfile::tempdir().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
        assert_eq!(
            view_state::table
                .select(view_state::id)
                .load::<i32>(&mut creating)
                .expect("initial view"),
            [40]
        );
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("restored column identities"));
    assert_eq!(
        view_state::table.select(view_state::id).load::<i32>(&mut reopened).expect("replayed view"),
        [40]
    );
}

#[test]
fn rolled_back_names_preserve_a_surviving_view() {
    assert_replayed_view(
        "CREATE TABLE ledger(id INT PRIMARY KEY, value INT NOT NULL);
         INSERT INTO ledger VALUES(1,40);
         CREATE TEMP VIEW view_state AS SELECT value AS id FROM ledger;
         BEGIN;
         ALTER TABLE ledger RENAME COLUMN value TO amount;
         ALTER TABLE ledger RENAME TO retired;
         ROLLBACK;",
    );
}

#[test]
fn rolled_back_column_replacement_preserves_a_surviving_view() {
    assert_replayed_view(
        "CREATE TABLE ledger(id INT PRIMARY KEY, value INT NOT NULL);
         INSERT INTO ledger VALUES(1,40);
         CREATE TEMP VIEW view_state AS SELECT value AS id FROM ledger;
         BEGIN;
         DROP VIEW view_state;
         ALTER TABLE ledger DROP COLUMN value;
         ALTER TABLE ledger ADD COLUMN amount INT NOT NULL DEFAULT 99;
         ROLLBACK;",
    );
}
