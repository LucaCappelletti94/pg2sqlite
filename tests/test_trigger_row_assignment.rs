//! A plpgsql trigger assigning to `NEW.<column>`, which changes the row being
//! written.
//!
//! SQLite has no writable `NEW`, so the assignment is emulated with an
//! `UPDATE ... WHERE rowid = NEW.rowid` over the row just written. The
//! assigned value is PostgreSQL and has to be translated like any other
//! expression, which is what these tests execute.
//!
//! Expected values measured on PostgreSQL 16 with the session zone at UTC:
//! `json_typeof('1'::json)` is `number`, `greatest(0, 1)` is 1,
//! `'abc' ILIKE 'A%'` is true, and `'2023-01-15 12:00:00'::timestamp AT TIME
//! ZONE 'UTC'` read back as a timestamp is unchanged.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

/// Builds a script whose trigger function body is `body`, writes one row, and
/// returns `column` from it.
///
/// The row matters: SQLite does not resolve function names when a trigger is
/// created, so an untranslated `greatest` survives the DDL and only fails on
/// the write.
fn write_and_read(columns: &str, body: &str, values: &str, column: &str) -> Option<String> {
    run_translated_with(
        &format!(
            "CREATE TABLE t ({columns});
             CREATE FUNCTION f() RETURNS TRIGGER AS $$
             BEGIN
                 {body}
                 RETURN NEW;
             END;
             $$ LANGUAGE plpgsql;
             CREATE TRIGGER tr BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();
             INSERT INTO t VALUES {values};
             SELECT {column} FROM t;"
        ),
        &Pg2SqliteOptions::default(),
    )
    .into_iter()
    .next()
    .expect("one row")
}

/// Translates `pg`, asserts the refusal names `target` as the thing being
/// assigned to, and returns the message so a caller can pin which of the two
/// wordings it got.
///
/// Also asserts the parser's own complaint is gone. Before this rule existed
/// the rewrite produced `NEW.SET txt = ...` and the failure read
/// `Expected: an SQL statement, found: NEW`, whose body dump happens to contain
/// the target too, so an assertion that only looked for the target would pass
/// on the old behaviour.
fn assert_refuses(pg: &str, target: &str) -> String {
    let message = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("an assignment to a qualified target outside the recognised shape")
        .to_string();

    assert!(
        message.contains(&format!("assigning to `{target}`")),
        "the refusal should name the assignment target: {message}"
    );
    assert!(
        !message.contains("Expected: an SQL statement"),
        "the refusal should replace the parser's complaint, not sit behind it: {message}"
    );
    message
}

/// The wording for a target that decides what gets written, which is `NEW`.
const WRITTEN_ROW: &str = "cannot change the row it fired for";

/// The wording for a target that is a plpgsql local, which is everything else.
const RECORD_LOCAL: &str = "no plpgsql record variable to hold the change";

/// A trigger function whose body is `body`, on a table with a text column.
fn script_with_body(body: &str) -> String {
    format!(
        "CREATE TABLE t (id INT PRIMARY KEY, txt TEXT);
         CREATE FUNCTION f() RETURNS TRIGGER AS $$
         BEGIN
             {body}
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER tr BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();"
    )
}

#[test]
fn a_timezone_conversion_in_a_row_assignment_is_translated() {
    assert_eq!(
        write_and_read(
            "id INT PRIMARY KEY, naive TIMESTAMP",
            "NEW.naive := NEW.naive AT TIME ZONE 'UTC';",
            "(1, '2023-01-15 12:00:00')",
            "naive",
        ),
        Some("2023-01-15 12:00:00".to_string()),
    );
}

#[test]
fn a_cast_operator_in_a_row_assignment_is_translated() {
    assert_eq!(
        write_and_read(
            "id INT PRIMARY KEY, txt TEXT",
            "NEW.txt := json_typeof('1'::json);",
            "(1, 'x')",
            "txt",
        ),
        Some("number".to_string()),
    );
}

#[test]
fn a_renamed_function_in_a_row_assignment_is_translated() {
    assert_eq!(
        write_and_read("id INT PRIMARY KEY, n INT", "NEW.n := greatest(NEW.n, 1);", "(1, 0)", "n",),
        Some("1".to_string()),
    );
}

#[test]
fn a_case_expression_in_a_row_assignment_is_translated() {
    assert_eq!(
        write_and_read(
            "id INT PRIMARY KEY, txt TEXT",
            "NEW.txt := (CASE WHEN NEW.txt ILIKE 'A%' THEN 'y' ELSE 'n' END);",
            "(1, 'abc')",
            "txt",
        ),
        Some("y".to_string()),
    );
}

/// The UPDATE branch, which the four tests above do not reach. It emits a
/// different shape, `BEFORE UPDATE OF <other columns>` anchored on `OLD.rowid`
/// rather than `AFTER INSERT` anchored on `NEW.rowid`, and it runs the same
/// translation, so a fix that only reached the insert path would pass every
/// test above and fail here.
#[test]
fn a_row_assignment_on_the_update_branch_is_translated() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT, txt TEXT);
         CREATE FUNCTION f() RETURNS TRIGGER AS $$
         BEGIN
             NEW.txt := (CASE WHEN NEW.txt ILIKE 'A%' THEN 'y' ELSE 'n' END);
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER tr BEFORE UPDATE ON t FOR EACH ROW EXECUTE FUNCTION f();
         INSERT INTO t VALUES (1, 0, 'abc');
         UPDATE t SET n = 1 WHERE id = 1;
         SELECT txt FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("y".to_string())], "ILIKE must be translated on the update branch");
}

/// Guards the fix rather than testing it. A `DECLARE` variable runs through the
/// plpgsql translator instead of the row-assignment path and already
/// translates, so it must keep doing so.
#[test]
fn a_declared_variable_assignment_still_translates() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT);
         CREATE TABLE seen (id INT PRIMARY KEY, n INT);
         CREATE FUNCTION f() RETURNS TRIGGER AS $$
         DECLARE
             doubled INT;
         BEGIN
             doubled := greatest(NEW.n, 1) * 2;
             INSERT INTO seen (id, n) VALUES (NEW.id, doubled);
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER tr AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();
         INSERT INTO t VALUES (1, 0);
         SELECT n FROM seen;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("2".to_string())], "greatest(0, 1) * 2 is 2");
}

#[test]
fn an_assignment_inside_an_if_is_refused() {
    let message = assert_refuses(
        &script_with_body("IF NEW.txt IS NULL THEN NEW.txt := 'fallback'; END IF;"),
        "NEW.txt",
    );
    assert!(message.contains(WRITTEN_ROW), "NEW decides what is written: {message}");
}

/// PostgreSQL accepts a bare `=` as a plpgsql assignment, measured on
/// PostgreSQL 16, so the refusal covers both spellings.
#[test]
fn an_assignment_inside_an_if_spelled_with_one_equals_is_refused() {
    assert_refuses(
        &script_with_body("IF NEW.txt IS NULL THEN NEW.txt = 'fallback'; END IF;"),
        "NEW.txt",
    );
}

#[test]
fn an_assignment_beside_another_statement_is_refused() {
    assert_refuses(
        &format!(
            "CREATE TABLE audit (note TEXT);
             {}",
            script_with_body(
                "NEW.txt := upper(NEW.txt); INSERT INTO audit (note) VALUES ('touched');"
            )
        ),
        "NEW.txt",
    );
}

/// This body already is a run of assignments closed by `RETURN NEW`, so the
/// refusal has to name the other half of the rule, that the column be one the
/// table declares, or it misdiagnoses the cause.
#[test]
fn an_assignment_to_an_undeclared_column_is_refused() {
    let message = assert_refuses(&script_with_body("NEW.nope := 'x';"), "NEW.nope");
    assert!(
        message.contains("columns the table declares"),
        "the refusal should name the declared-column requirement: {message}"
    );
}

/// `OLD` is a plpgsql record like any other, not a read only view of the row.
/// PostgreSQL accepts the assignment and runs the write, measured on
/// PostgreSQL 16, because a BEFORE trigger writes what `RETURN NEW` hands back.
/// The same prefix split mangles it into `OLD.SET txt = ...`, so it is refused,
/// and it takes the record wording rather than the written-row wording.
#[test]
fn an_assignment_to_the_old_row_is_refused() {
    let message = assert_refuses(&script_with_body("OLD.txt := 'x';"), "OLD.txt");
    assert!(message.contains(RECORD_LOCAL), "OLD does not decide what is written: {message}");
}

/// A field of a record variable takes the same mangling, into `rec.SET a = 1`,
/// and the same wording as `OLD`.
#[test]
fn an_assignment_to_a_record_field_is_refused() {
    let message = assert_refuses(
        "CREATE TABLE t (id INT PRIMARY KEY, txt TEXT);
         CREATE FUNCTION f() RETURNS TRIGGER AS $$
         DECLARE
             rec RECORD;
         BEGIN
             rec.a := 1;
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER tr BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();",
        "rec.a",
    );
    assert!(message.contains(RECORD_LOCAL), "a record field is a plpgsql local: {message}");
}

/// A plpgsql `CASE` statement, which is not the `CASE` expression the test
/// above guards. Two of its three spellings are refused, and which two is not
/// obvious, so both are pinned. The third, a bare `=` in a `CASE` statement
/// with no `ELSE` arm, is the one shape the rule misses: its `THEN` sits inside
/// an open `CASE` and the closing chunk carries only `END CASE`, so it keeps
/// the parser's own complaint. That is a miss, never a wrong refusal.
#[test]
fn an_assignment_inside_a_case_statement_is_refused() {
    for body in [
        "CASE WHEN NEW.txt IS NULL THEN NEW.txt = 'a'; ELSE NEW.txt = 'b'; END CASE;",
        "CASE WHEN NEW.txt IS NULL THEN NEW.txt := 'a'; END CASE;",
    ] {
        let message = assert_refuses(&script_with_body(body), "NEW.txt");
        assert!(message.contains(WRITTEN_ROW), "body `{body}`: {message}");
    }
}

/// A comparison is not an assignment. `WHERE t.id = NEW.id` in a trigger body
/// carries a qualified name in front of an `=` and must translate, which is
/// what separates the rule above from a blanket ban on the shape.
#[test]
fn a_qualified_comparison_in_a_trigger_body_still_translates() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT);
         CREATE TABLE seen (id INT PRIMARY KEY, n INT);
         CREATE FUNCTION f() RETURNS TRIGGER AS $$
         BEGIN
             INSERT INTO seen (id, n) SELECT t.id, t.n FROM t WHERE t.id = NEW.id;
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER tr AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();
         INSERT INTO t VALUES (1, 7);
         SELECT n FROM seen;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("7".to_string())], "the comparison is not an assignment");
}

/// A `CASE` arm's `THEN` and `ELSE` are not statement starts, so a qualified
/// equality inside one stays a comparison. The first version of the rule read
/// them as branches and refused this, which is what the assertion catches.
#[test]
fn a_qualified_equality_inside_a_case_arm_still_translates() {
    for arm in ["WHEN NEW.n > 0 THEN t.n = 1 ELSE FALSE", "WHEN NEW.n < 0 THEN FALSE ELSE t.n = 1"]
    {
        let rows = run_translated_with(
            &format!(
                "CREATE TABLE t (id INT PRIMARY KEY, n INT);
                 CREATE TABLE audit (id INT PRIMARY KEY, flag BOOLEAN);
                 CREATE FUNCTION f() RETURNS TRIGGER AS $$
                 BEGIN
                     INSERT INTO audit (id, flag)
                     SELECT t.id, CASE {arm} END FROM t;
                     RETURN NEW;
                 END;
                 $$ LANGUAGE plpgsql;
                 CREATE TRIGGER tr AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f();
                 INSERT INTO t VALUES (1, 1);
                 SELECT flag FROM audit;"
            ),
            &Pg2SqliteOptions::default(),
        );
        assert_eq!(rows, vec![Some("1".to_string())], "arm `{arm}` should compare, not assign");
    }
}
