//! Coverage tests for single-branch paths in the value-fidelity patch that
//! the rest of the suite does not reach.  Each test exercises exactly one such
//! branch and asserts either the refusal message or the row value.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation};

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;
use run_translated_helper::run_translated_with;

fn text_uuid_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text)
}

// ── uuid.rs:233 — non-literal passes through
// maybe_canonicalize_text_uuid_literal ──

/// A column reference at a UUID-Text INSERT position is not a string literal,
/// so `maybe_canonicalize_text_uuid_literal` returns it unchanged (line 233).
/// Exercises the `None` branch of `single_quoted_literal`.
#[test]
fn uuid_text_insert_select_non_literal_passes_through() {
    // INSERT ... SELECT where the UUID column value is a column reference, not
    // a string literal; the function must pass it through without error.
    let rows = run_translated_with(
        "CREATE TABLE src (u TEXT NOT NULL);
         INSERT INTO src VALUES ('550e8400-e29b-41d4-a716-446655440000');
         CREATE TABLE dst (u TEXT NOT NULL);
         INSERT INTO dst (u) SELECT u FROM src;
         SELECT u FROM dst;",
        &text_uuid_opts(),
    );
    assert_eq!(rows, vec![Some("550e8400-e29b-41d4-a716-446655440000".to_string())]);
}

/// CAST(column AS UUID) under Text representation: the column reference is not
/// a literal so `maybe_canonicalize_text_uuid_literal` returns it unchanged
/// (expr.rs line 2484 path exercised, then uuid.rs:233 hits the None branch).
#[test]
fn cast_uuid_column_to_uuid_text_passes_through() {
    let rows = run_translated_with(
        "CREATE TABLE t (u TEXT NOT NULL);
         INSERT INTO t VALUES ('550e8400-e29b-41d4-a716-446655440000');
         SELECT CAST(u AS UUID) FROM t;",
        &text_uuid_opts(),
    );
    assert_eq!(rows, vec![Some("550e8400-e29b-41d4-a716-446655440000".to_string())]);
}

/// CAST(literal AS UUID) under Text representation canonicalises the literal
/// (expr.rs line 2484; the literal IS a string literal so canonicalisation
/// runs).
#[test]
fn cast_uuid_literal_to_uuid_text_canonicalizes() {
    // Uppercase UUID → canonical lowercase hyphenated form.
    let rows = run_translated_with(
        "SELECT CAST('550E8400-E29B-41D4-A716-446655440000' AS UUID);",
        &text_uuid_opts(),
    );
    assert_eq!(rows, vec![Some("550e8400-e29b-41d4-a716-446655440000".to_string())]);
}

// ── column.rs:336 — character_length branch in declared_bound_checks
// ──────────

/// `CHAR(n)` emits a `CHECK(length(col) <= n)` constraint; inserting a longer
/// value must be refused at execute time, exercising the character_length
/// branch at column.rs:336.
#[test]
fn char_column_length_check_is_enforced() {
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().unwrap();
    let stmts = Pg2Sqlite::default()
        .sql("CREATE TABLE t (col CHAR(3) NOT NULL);")
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate");
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap_or_else(|e| panic!("DDL failed: {e}\n{s}"));
    }
    conn.execute_batch("INSERT INTO t VALUES ('ab');").expect("2-char value fits in CHAR(3)");
    let err = conn
        .execute_batch("INSERT INTO t VALUES ('abcd');")
        .expect_err("4-char value must be rejected by the CHAR(3) CHECK");
    assert!(err.to_string().contains("CHECK"), "rejection must come from CHECK, got: {err}");
}

// ── options.rs:444 — duplicate trigger name guard
// ─────────────────────────────

/// A trigger with both INSERT and UPDATE events appears in two conflict groups
/// during the pre-walk.  `add_conflicting_trigger_name` is called twice per
/// trigger name; the guard at options.rs:444 ensures the name is not added a
/// second time.  Observable: refusal cites "multiple BEFORE/AFTER" (the
/// conflict message), not an internal duplication error.
#[test]
fn conflicting_trigger_name_not_added_twice() {
    let err = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY);
             CREATE FUNCTION f1() RETURNS trigger AS $$ BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql;
             CREATE FUNCTION f2() RETURNS trigger AS $$ BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql;
             CREATE TRIGGER t1 BEFORE INSERT OR UPDATE ON t FOR EACH ROW EXECUTE FUNCTION f1();
             CREATE TRIGGER t2 BEFORE INSERT OR UPDATE ON t FOR EACH ROW EXECUTE FUNCTION f2();",
        )
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("conflicting triggers must be refused");
    assert!(
        err.to_string().contains("multiple BEFORE/AFTER"),
        "expected conflict refusal, got: {err}"
    );
}

// ── pg2sqlite.rs:136 — TriggerEvent::Truncate skipped in conflict detection ──

/// Two BEFORE TRUNCATE triggers on the same table are NOT counted as
/// conflicting (pg2sqlite.rs:136 skips TRUNCATE events in the loop).
/// Both are refused as "TRUNCATE trigger has no SQLite equivalent", not as
/// "multiple BEFORE/AFTER triggers".
#[test]
fn truncate_trigger_refused_not_as_conflicting() {
    let err = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY);
             CREATE FUNCTION f1() RETURNS trigger AS $$ BEGIN RETURN NULL; END; $$ LANGUAGE plpgsql;
             CREATE FUNCTION f2() RETURNS trigger AS $$ BEGIN RETURN NULL; END; $$ LANGUAGE plpgsql;
             CREATE TRIGGER t1 BEFORE TRUNCATE ON t FOR EACH STATEMENT EXECUTE FUNCTION f1();
             CREATE TRIGGER t2 BEFORE TRUNCATE ON t FOR EACH STATEMENT EXECUTE FUNCTION f2();",
        )
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("TRUNCATE triggers must be refused");
    let msg = err.to_string();
    assert!(msg.contains("TRUNCATE"), "refusal must mention TRUNCATE, got: {msg}");
    assert!(
        !msg.contains("multiple BEFORE/AFTER"),
        "TRUNCATE events must not count toward conflict detection, got: {msg}"
    );
}

// ── pg2sqlite.rs:123 — INSTEAD OF period skipped in conflict detection
// ────────

/// An INSTEAD OF trigger has period = InsteadOf, which is excluded from
/// conflict detection at pg2sqlite.rs:123.  Even two INSTEAD OF INSERT
/// triggers on the same view must not be refused as "multiple BEFORE/AFTER".
#[test]
fn instead_of_trigger_not_refused_as_conflicting() {
    let result = Pg2Sqlite::default()
        .sql(
            "CREATE VIEW v AS SELECT 1 AS id;
             CREATE FUNCTION f1() RETURNS trigger AS $$ BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql;
             CREATE FUNCTION f2() RETURNS trigger AS $$ BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql;
             CREATE TRIGGER t1 INSTEAD OF INSERT ON v FOR EACH ROW EXECUTE FUNCTION f1();
             CREATE TRIGGER t2 INSTEAD OF INSERT ON v FOR EACH ROW EXECUTE FUNCTION f2();",
        )
        .expect("parse")
        .translate(&Pg2SqliteOptions::default());
    // The triggers may fail for other reasons but MUST NOT be flagged as
    // "multiple BEFORE/AFTER" since INSTEAD OF is excluded from the counter.
    if let Err(ref e) = result {
        assert!(
            !e.to_string().contains("multiple BEFORE/AFTER"),
            "INSTEAD OF triggers must not be counted as BEFORE/AFTER conflicts, got: {e}"
        );
    }
}
