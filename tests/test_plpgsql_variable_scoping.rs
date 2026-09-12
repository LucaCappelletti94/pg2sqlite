//! What a PL/pgSQL variable holds on each side of an `IF`, proved by firing
//! the emitted trigger.
//!
//! PostgreSQL scopes a variable to the block that declares it, not to the
//! `IF` that reads it, so an assignment before an `IF` is visible inside every
//! branch, and an assignment inside a branch only took effect if that branch
//! ran. The emitted trigger has no control flow: each statement carries its
//! branch condition as a guard, so a value assigned inside a branch has to
//! reach later reads as a conditional value rather than as itself.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

/// Translates a trigger function over `body`, fires it for a row with
/// `t.val = val`, and returns the `log` rows as `note|n` with `NULL` spelled
/// out.
///
/// # Panics
///
/// Panics when translation fails, when an emitted statement will not execute,
/// or when the write that fires the trigger is rejected.
fn log_after_firing(body: &str, val: i64) -> Vec<String> {
    let pg = format!(
        "
CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER);
CREATE TABLE log (id INTEGER PRIMARY KEY, note TEXT, n INTEGER);
CREATE OR REPLACE FUNCTION f() RETURNS TRIGGER AS $$
DECLARE
    v_n INTEGER;
BEGIN
{body}
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER tr AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();
"
    );

    let statements = Pg2Sqlite::default()
        .sql(&pg)
        .expect("fixture parses")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("fixture translates");

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted statement failed: {error}\n{statement}"));
    }
    connection
        .execute_batch("INSERT INTO log (id, note, n) VALUES (1, 'seed', 0);")
        .expect("seed row");
    connection
        .execute_batch(&format!("INSERT INTO t (id, val) VALUES (1, {val});"))
        .expect("the write that fires the trigger");

    let mut prepared =
        connection.prepare("SELECT note, n FROM log ORDER BY id").expect("probe prepares");
    prepared
        .query_map([], |row| {
            Ok(format!(
                "{}|{}",
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?.map_or_else(|| "NULL".to_string(), |n| n.to_string())
            ))
        })
        .expect("probe runs")
        .collect::<Result<Vec<_>, _>>()
        .expect("probe rows decode")
}

#[test]
fn an_assignment_before_an_if_is_read_inside_it() {
    assert_eq!(
        log_after_firing(
            "    v_n := NEW.val * 2;
    IF NEW.val > 0 THEN
      INSERT INTO log (id, note, n) VALUES (2, 'inside', v_n);
    END IF;",
            3
        ),
        vec!["seed|0".to_string(), "inside|6".to_string()]
    );
}

#[test]
fn an_assignment_before_a_nested_if_is_read_inside_it() {
    assert_eq!(
        log_after_firing(
            "    v_n := NEW.val * 2;
    IF NEW.val > 0 THEN
      IF NEW.val < 100 THEN
        INSERT INTO log (id, note, n) VALUES (2, 'nested', v_n);
      END IF;
    END IF;",
            3
        ),
        vec!["seed|0".to_string(), "nested|6".to_string()]
    );
}

#[test]
fn an_assignment_before_an_if_is_read_by_a_guarded_update() {
    assert_eq!(
        log_after_firing(
            "    v_n := NEW.val * 2;
    IF NEW.val > 0 THEN
      UPDATE log SET n = v_n WHERE id = 1;
    END IF;",
            3
        ),
        vec!["seed|6".to_string()]
    );
}

#[test]
fn an_assignment_inside_a_branch_that_did_not_run_is_not_read_after_it() {
    assert_eq!(
        log_after_firing(
            "    IF NEW.val > 10 THEN
      v_n := 99;
    END IF;
    INSERT INTO log (id, note, n) VALUES (2, 'after', v_n);",
            3
        ),
        vec!["seed|0".to_string(), "after|NULL".to_string()]
    );
}

#[test]
fn an_assignment_inside_a_branch_that_ran_is_read_after_it() {
    assert_eq!(
        log_after_firing(
            "    IF NEW.val > 10 THEN
      v_n := 99;
    END IF;
    INSERT INTO log (id, note, n) VALUES (2, 'after', v_n);",
            15
        ),
        vec!["seed|0".to_string(), "after|99".to_string()]
    );
}

#[test]
fn an_assignment_inside_a_branch_does_not_reach_its_sibling() {
    assert_eq!(
        log_after_firing(
            "    IF NEW.val > 10 THEN
      v_n := 99;
    ELSE
      INSERT INTO log (id, note, n) VALUES (2, 'sibling', v_n);
    END IF;",
            3
        ),
        vec!["seed|0".to_string(), "sibling|NULL".to_string()]
    );
}

#[test]
fn each_branch_contributes_its_own_value_to_a_later_read() {
    let body = "    IF NEW.val > 10 THEN
      v_n := 99;
    ELSIF NEW.val > 0 THEN
      v_n := 7;
    END IF;
    INSERT INTO log (id, note, n) VALUES (2, 'after', v_n);";

    assert_eq!(log_after_firing(body, 15), vec!["seed|0".to_string(), "after|99".to_string()]);
    assert_eq!(log_after_firing(body, 3), vec!["seed|0".to_string(), "after|7".to_string()]);
    assert_eq!(log_after_firing(body, -1), vec!["seed|0".to_string(), "after|NULL".to_string()]);
}

#[test]
fn a_branch_assignment_overwrites_an_earlier_one_only_when_it_ran() {
    let body = "    v_n := 1;
    IF NEW.val > 10 THEN
      v_n := 99;
    END IF;
    INSERT INTO log (id, note, n) VALUES (2, 'after', v_n);";

    assert_eq!(log_after_firing(body, 15), vec!["seed|0".to_string(), "after|99".to_string()]);
    assert_eq!(log_after_firing(body, 3), vec!["seed|0".to_string(), "after|1".to_string()]);
}
