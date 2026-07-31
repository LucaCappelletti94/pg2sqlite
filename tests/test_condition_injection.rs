//! Guards injected into a statement that already has a WHERE clause.
//!
//! A plpgsql `IF` around a DML statement becomes a condition appended to that
//! statement's WHERE. Appended as a bare `existing AND guard`, an `existing`
//! with a top level OR reassociates, because AND binds tighter:
//! `id = 1 OR id = 2 AND guard` is `id = 1 OR (id = 2 AND guard)`, and the
//! guard never reaches the first disjunct.
//!
//! Measured on SQLite over rows `(1, alice)`, `(2, bob)`, `(3, alice)`: with a
//! guard that is false for every row, `DELETE ... WHERE id = 1 OR id = 2`
//! should remove nothing, and unparenthesised it removes row 1.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::Pg2SqliteOptions;
use run_translated_helper::run_translated_with;

/// A trigger whose body wraps `body` in an `IF`, which is what appends the
/// guard to the statement's WHERE clause.
fn after(body: &str, projection: &str) -> Vec<Option<String>> {
    run_translated_with(
        &format!(
            "CREATE TABLE t (id INT PRIMARY KEY, owner TEXT);
             CREATE TABLE audit (id INT PRIMARY KEY);
             CREATE FUNCTION guarded() RETURNS trigger AS $$
             BEGIN
                 IF NEW.id < 0 THEN
                     {body}
                 END IF;
                 RETURN NEW;
             END;
             $$ LANGUAGE plpgsql;
             CREATE TRIGGER go AFTER INSERT ON audit FOR EACH ROW
                 EXECUTE FUNCTION guarded();
             INSERT INTO t VALUES (1, 'alice'), (2, 'bob'), (3, 'alice');
             INSERT INTO audit VALUES (1);
             SELECT group_concat({projection}) FROM (SELECT id, owner FROM t ORDER BY id);"
        ),
        &Pg2SqliteOptions::default(),
    )
}

/// The guard has to bind to the whole existing condition, not just its last
/// disjunct. Row 1 is owned by alice and the guard only permits bob, so it must
/// survive.
#[test]
fn a_guard_binds_to_an_or_condition_as_a_whole() {
    assert_eq!(
        after("DELETE FROM t WHERE id = 1 OR id = 2;", "id"),
        vec![Some("1,2,3".to_string())],
        "the guard is false, so nothing may be deleted"
    );
}

/// The same for UPDATE, where a leak rewrites a row rather than removing it.
#[test]
fn an_update_guard_binds_to_an_or_condition_as_a_whole() {
    assert_eq!(
        after("UPDATE t SET owner = 'taken' WHERE id = 1 OR id = 2;", "owner"),
        vec![Some("alice,bob,alice".to_string())],
        "the guard is false, so no owner may change"
    );
}
