//! Refusal guards for expression lowerings that copy an operand (C7–C12).
//!
//! Several lowerings write one operand into multiple output positions, so a
//! volatile operand is evaluated more often than PostgreSQL evaluates it and
//! the copies disagree. Each guard checks the copied operand with
//! `is_replayable` before emitting. A stable operand continues to translate
//! and execute correctly.
//!
//! `c()` is the volatile probe for refusal tests: unknown to the crate, so
//! `is_replayable` conservatively returns false, and the construct-specific
//! guard fires before the function translator can complain. `random()` is
//! the probe for the "should not refuse" companions: volatile and known, so
//! it translates but still keeps `is_replayable` false.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::ArrayRepresentation,
};

#[path = "helpers/run_translated.rs"]
mod run_translated;
use run_translated::run_translated_with;

fn array_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json)
}

// ── C7: SUBSTRING(s FROM volatile FOR n) ────────────────────────────────────

/// A volatile FROM expression is copied into both the start and the length
/// adjustment; refuse so the copies cannot disagree.
#[test]
fn substring_volatile_from_is_refused() {
    let err = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, s TEXT NOT NULL); \
             SELECT SUBSTRING(s FROM c() FOR 3) FROM t;",
        )
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("volatile FROM should be refused");
    let msg = err.to_string();
    assert!(msg.contains("SUBSTRING"), "error names construct: {msg}");
}

/// A stable FROM expression translates and executes; PostgreSQL answers 'ell'
/// for SUBSTRING('hello' FROM 2 FOR 3).
#[test]
fn substring_stable_from_executes_correctly() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT NOT NULL); \
         INSERT INTO t VALUES (1, 'hello'); \
         SELECT SUBSTRING(s FROM 2 FOR 3) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("ell".to_string())]);
}

/// No FOR clause means no length adjustment, so the FROM expression is not
/// copied: a volatile FROM is fine there.
#[test]
fn substring_without_for_volatile_from_still_translates() {
    Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, s TEXT NOT NULL); \
             SELECT SUBSTRING(s FROM random()) FROM t;",
        )
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("FROM-only form does not copy the expression");
}

// ── C8: s ^@ volatile_prefix ─────────────────────────────────────────────────

/// The prefix is written into `length(p)` and again into the equality
/// comparison; refuse when it is volatile.
#[test]
fn starts_with_volatile_prefix_is_refused() {
    let err = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, s TEXT NOT NULL); \
             SELECT s ^@ c() FROM t;",
        )
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("volatile prefix should be refused");
    let msg = err.to_string();
    assert!(msg.contains("^@"), "error names construct: {msg}");
}

/// A stable prefix translates and executes; PostgreSQL answers row 1 for
/// 'hello' ^@ 'hel'.
#[test]
fn starts_with_stable_prefix_executes_correctly() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT NOT NULL); \
         INSERT INTO t VALUES (1, 'hello'), (2, 'world'); \
         SELECT id FROM t WHERE s ^@ 'hel';",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
}

// ── C9: OVERLAY(volatile PLACING repl FROM pos) ──────────────────────────────

/// The source string is cloned into the prefix and the suffix substr calls;
/// refuse when it is volatile.
#[test]
fn overlay_volatile_source_is_refused() {
    let err = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY); \
             SELECT OVERLAY(c() PLACING 'X' FROM 1) FROM t;",
        )
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("volatile source should be refused");
    let msg = err.to_string();
    assert!(msg.contains("OVERLAY"), "error names construct: {msg}");
}

/// A stable source translates and executes; PostgreSQL answers 'hXYlo' for
/// OVERLAY('hello' PLACING 'XY' FROM 2 FOR 2).
#[test]
fn overlay_stable_source_executes_correctly() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT NOT NULL); \
         INSERT INTO t VALUES (1, 'hello'); \
         SELECT OVERLAY(s PLACING 'XY' FROM 2 FOR 2) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("hXYlo".to_string())]);
}

// ── C10: doc #> '{numeric,...}' with volatile doc ────────────────────────────

/// A numeric path element emits COALESCE(doc -> index, doc -> 'index'), which
/// reads doc twice; refuse when doc is volatile.
#[test]
fn json_path_volatile_doc_with_numeric_hop_is_refused() {
    let err = Pg2Sqlite::default()
        .sql("SELECT c() #> '{0}';")
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("volatile doc with numeric path hop should be refused");
    let msg = err.to_string();
    assert!(msg.contains("#>"), "error names construct: {msg}");
}

/// A stable doc with a numeric path element translates and executes; PostgreSQL
/// answers 20 for `[10,20,30] #> '{1}'`.
#[test]
fn json_path_stable_doc_with_numeric_hop_executes_correctly() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, doc TEXT NOT NULL); \
         INSERT INTO t VALUES (1, '[10,20,30]'); \
         SELECT doc #> '{1}' FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("20".to_string())]);
}

/// A string-only path never clones the document, so a volatile doc is fine.
#[test]
fn json_path_string_key_path_volatile_doc_does_not_refuse() {
    Pg2Sqlite::default()
        .sql("SELECT random() #> '{a,b}';")
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("string-key path should not refuse volatile doc");
}

// ── C11: doc ?& ARRAY[k1,k2] and doc ?| ARRAY[k1,k2] ───────────────────────

/// Two or more keys clone the document once per key; refuse when doc is
/// volatile.
#[test]
fn question_and_volatile_doc_multiple_keys_is_refused() {
    let err = Pg2Sqlite::default()
        .sql("SELECT c() ?& ARRAY['k1','k2'];")
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("volatile doc with 2+ keys should be refused");
    let msg = err.to_string();
    assert!(msg.contains("?&"), "error names construct: {msg}");
}

/// A stable doc with two keys translates and executes; PostgreSQL answers true
/// (1) when both keys exist.
#[test]
fn question_and_stable_doc_multiple_keys_executes_correctly() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, doc TEXT NOT NULL); \
         INSERT INTO t VALUES (1, '{\"k1\":\"v1\",\"k2\":\"v2\"}'); \
         SELECT (doc ?& ARRAY['k1','k2']) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
}

/// A single key writes the document once; no refusal even for a volatile doc.
#[test]
fn question_and_single_key_volatile_doc_does_not_refuse() {
    Pg2Sqlite::default()
        .sql("SELECT random() ?& ARRAY['k1'];")
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("one-key form does not copy the document");
}

/// Two or more keys clone the document once per key; refuse when doc is
/// volatile.
#[test]
fn question_pipe_volatile_doc_multiple_keys_is_refused() {
    let err = Pg2Sqlite::default()
        .sql("SELECT c() ?| ARRAY['k1','k2'];")
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("volatile doc with 2+ keys should be refused");
    let msg = err.to_string();
    assert!(msg.contains("?|"), "error names construct: {msg}");
}

/// A stable doc with two keys translates and executes; PostgreSQL answers true
/// (1) when any key exists.
#[test]
fn question_pipe_stable_doc_multiple_keys_executes_correctly() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, doc TEXT NOT NULL); \
         INSERT INTO t VALUES (1, '{\"k1\":\"v1\"}'); \
         SELECT (doc ?| ARRAY['k1','missing']) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
}

// ── C12: volatile = ANY(array_column) ───────────────────────────────────────

/// The left side is placed inside the json_each WHERE clause and evaluated
/// once per array element; refuse when it is volatile.
#[test]
fn any_volatile_left_over_array_column_is_refused() {
    let err = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, tags TEXT[]); \
             SELECT id FROM t WHERE c() = ANY(tags);",
        )
        .unwrap()
        .translate_to_sql(&array_opts())
        .expect_err("volatile left over array column should be refused");
    let msg = err.to_string();
    assert!(msg.contains("ANY"), "error names construct: {msg}");
}

/// A stable left side translates and executes; PostgreSQL answers row 1 when
/// 'b' is in the tags array.
#[test]
fn any_stable_left_over_array_column_executes_correctly() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, tags TEXT[]); \
         INSERT INTO t VALUES (1, ARRAY['a','b','c']); \
         SELECT id FROM t WHERE 'b' = ANY(tags);",
        &array_opts(),
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
}

// ── ||/ coordinated with CopyFunctions ───────────────────────────────────────

/// The cube-root closed form reads the operand for its sign and again for its
/// magnitude; refuse when the operand is volatile.
#[test]
fn cube_root_op_volatile_operand_is_refused() {
    let err = Pg2Sqlite::default()
        .sql("SELECT ||/ c();")
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default().with_math_functions_available())
        .expect_err("volatile operand should be refused");
    let msg = err.to_string();
    assert!(msg.contains("||/"), "error names construct: {msg}");
}

/// A literal operand is replayable; the guard should not fire and the
/// expression should translate without error.
#[test]
fn cube_root_op_stable_operand_translates_without_error() {
    Pg2Sqlite::default()
        .sql("SELECT ||/ 8;")
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default().with_math_functions_available())
        .expect("stable operand should translate");
}
