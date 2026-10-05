//! `IS [NOT] JSON` translation onto SQLite's `json1` predicates.
//!
//! Every assertion executes the translator's own output against an in-memory
//! SQLite. `rusqlite` is used rather than diesel's typed DSL because the
//! statements under test are generated text whose shape is what is being
//! verified.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

/// The fixture every predicate test shares: one well-formed object, one
/// well-formed array, one scalar, and one string that is not JSON at all.
const FIXTURE: &str = r#"CREATE TABLE docs (id INT PRIMARY KEY, payload TEXT);
INSERT INTO docs (id, payload) VALUES (1, '{"a": 1}');
INSERT INTO docs (id, payload) VALUES (2, '[1, 2]');
INSERT INTO docs (id, payload) VALUES (3, '7');
INSERT INTO docs (id, payload) VALUES (4, 'not json');
"#;

fn translate(pg: &str) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    Pg2Sqlite::default().sql(pg)?.translate_to_sql(&Pg2SqliteOptions::default())
}

fn translate_ok(pg: &str) -> String {
    translate(pg).expect("translation should succeed").join(";\n")
}

fn reject(pg: &str) -> String {
    translate(pg).expect_err("translation should be rejected").to_string()
}

/// Translate the fixture plus `predicate`, run it, and return the matching ids.
fn matching_ids(predicate: &str) -> Vec<i64> {
    let mut statements =
        translate(&format!("{FIXTURE}SELECT id FROM docs WHERE {predicate} ORDER BY id;"))
            .expect("translation should succeed");
    let probe = statements.pop().expect("script should emit a query");

    let conn = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        conn.execute_batch(&format!("{statement};"))
            .unwrap_or_else(|e| panic!("emitted setup failed: {e}\n{statement}"));
    }
    let mut stmt =
        conn.prepare(&probe).unwrap_or_else(|e| panic!("emitted probe failed: {e}\n{probe}"));
    stmt.query_map([], |row| row.get::<_, i64>(0))
        .unwrap_or_else(|e| panic!("emitted probe failed to run: {e}\n{probe}"))
        .collect::<Result<Vec<_>, _>>()
        .expect("ids should decode")
}

/// Before the predicate was translated it was emitted verbatim as
/// `payload IS JSON`, which SQLite cannot parse.
#[test]
fn is_json_is_never_emitted_verbatim() {
    let out = translate_ok(&format!("{FIXTURE}SELECT id FROM docs WHERE payload IS JSON;"));
    assert!(!out.contains("IS JSON"), "translated output must not contain IS JSON: {out}");
    matching_ids("payload IS JSON");
}

#[test]
fn is_json_becomes_json_valid() {
    let out = translate_ok(&format!("{FIXTURE}SELECT id FROM docs WHERE payload IS JSON;"));
    assert!(out.contains("json_valid(payload)"), "got: {out}");
    matching_ids("payload IS JSON");
}

#[test]
fn is_json_matches_every_well_formed_document() {
    assert_eq!(matching_ids("payload IS JSON"), vec![1, 2, 3]);
}

/// `IS JSON VALUE` is the explicit spelling of the unqualified form.
#[test]
fn is_json_value_matches_the_same_rows_as_is_json() {
    assert_eq!(matching_ids("payload IS JSON VALUE"), matching_ids("payload IS JSON"));
}

#[test]
fn is_not_json_negates_the_predicate() {
    let out = translate_ok(&format!("{FIXTURE}SELECT id FROM docs WHERE payload IS NOT JSON;"));
    assert!(out.contains("NOT (json_valid(payload))"), "got: {out}");
    assert_eq!(matching_ids("payload IS NOT JSON"), vec![4]);
}

/// `json_type` raises on malformed input, so the shape predicates must guard
/// with a `CASE` rather than an `AND`: SQLite does not promise to short-circuit
/// `AND`, and row 4 would abort the whole query if it reached `json_type`.
#[test]
fn shape_predicates_guard_json_type_behind_a_case() {
    let out = translate_ok(&format!("{FIXTURE}SELECT id FROM docs WHERE payload IS JSON ARRAY;"));
    assert!(out.contains("CASE WHEN json_valid(payload)"), "got: {out}");
    assert!(out.contains("json_type(payload) = 'array'"), "got: {out}");
    matching_ids("payload IS JSON ARRAY");
}

#[test]
fn is_json_array_matches_only_arrays() {
    assert_eq!(matching_ids("payload IS JSON ARRAY"), vec![2]);
}

#[test]
fn is_json_object_matches_only_objects() {
    assert_eq!(matching_ids("payload IS JSON OBJECT"), vec![1]);
}

#[test]
fn is_json_scalar_matches_neither_arrays_nor_objects() {
    assert_eq!(matching_ids("payload IS JSON SCALAR"), vec![3]);
}

#[test]
fn is_not_json_array_matches_everything_else() {
    assert_eq!(matching_ids("payload IS NOT JSON ARRAY"), vec![1, 3, 4]);
}

/// json1 keeps the last of a set of duplicate keys with no way to observe that
/// it did, so the uniqueness constraint cannot be answered rather than answered
/// wrongly.
#[test]
fn unique_keys_constraint_is_rejected() {
    for spelling in ["WITH UNIQUE KEYS", "WITHOUT UNIQUE KEYS"] {
        let err = reject(&format!(
            "{FIXTURE}SELECT id FROM docs WHERE payload IS JSON OBJECT {spelling};"
        ));
        assert!(err.contains("duplicate object keys"), "got: {err}");
    }
}
