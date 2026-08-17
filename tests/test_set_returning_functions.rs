//! A PostgreSQL set-returning function used where a table goes,
//! `SELECT ... FROM f(...)`.
//!
//! SQLite has almost none of these. The translator used to refuse exactly one
//! name, `generate_series`, and copy every other function out with its name and
//! arguments intact, so `SELECT * FROM json_array_elements('[1,2]')` translated
//! successfully and SQLite answered `no such table: json_array_elements`. The
//! guard was inside out: it read as though functions in `FROM` were supported
//! apart from one, when the truth is the reverse.
//!
//! The JSON family is the part that has a translation rather than only a
//! refusal, because every member is a projection over SQLite's `json_each`. The
//! quoting split is the trap, measured on PostgreSQL 16 and SQLite 3.51.1 over
//! `'["a",1]'`:
//!
//! | PostgreSQL | answers | SQLite |
//! |---|---|---|
//! | `json_array_elements` | `"a"`, `1` | `json_quote(value)` |
//! | `json_array_elements_text` | `a`, `1` | `value` |
//!
//! So the json-returning variants need the requote and the `_text` variants
//! must not have it, which is the opposite of what the shorter expression
//! suggests. `json_quote` is also correct for a nested element: over
//! `'[{"a":1}]'` both databases answer `{"a":1}` rather than a doubly quoted
//! string, because SQLite tracks the JSON subtype through `json_each`.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, TranslationOptions};
use run_translated_helper::run_translated_with;

const FIXTURE: &str = "CREATE TABLE t (id INT PRIMARY KEY, payload JSONB);
     INSERT INTO t VALUES (1, '[10,20]');";

/// Runs `query` against the fixture and returns the first column of every row.
fn rows(query: &str) -> Vec<Option<String>> {
    run_translated_with(&format!("{FIXTURE} {query};"), &Pg2SqliteOptions::default())
}

fn refuse(query: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{FIXTURE} {query};"))
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("this function has no SQLite table-valued form")
        .to_string()
}

/// The defect: a function with no SQLite form used to be emitted verbatim, and
/// the failure waited until the script was applied.
#[test]
fn a_function_with_no_sqlite_form_is_refused_naming_it() {
    for (query, name) in [
        ("SELECT * FROM regexp_split_to_table('a,b', ',')", "regexp_split_to_table"),
        ("SELECT * FROM json_to_recordset('[{\"a\":1}]')", "json_to_recordset"),
        ("SELECT * FROM some_extension_function(1)", "some_extension_function"),
    ] {
        let error = refuse(query);
        assert!(error.contains(name), "the error must name the function, got: {error}");
    }
}

/// `generate_series` keeps the message it already had, which suggests the
/// recursive CTE that replaces it. A generic refusal would be a regression.
#[test]
fn generate_series_keeps_its_specific_advice() {
    let error = refuse("SELECT * FROM generate_series(1, 3)");
    assert!(error.contains("RECURSIVE"), "the CTE suggestion must survive, got: {error}");
}

/// A function the caller registered on the destination passes through, which is
/// the same escape hatch scalar functions get.
#[test]
fn a_declared_function_passes_through() {
    let options =
        Pg2SqliteOptions::default().with_user_defined_functions(["some_extension_function"]);
    let translated = Pg2Sqlite::default()
        .sql(&format!("{FIXTURE} SELECT * FROM some_extension_function(1);"))
        .expect("parse")
        .translate(&options)
        .expect("a declared function is the caller's promise that it exists");
    assert!(!translated.is_empty(), "the document should translate into at least one statement");
}

/// The json-returning variants keep PostgreSQL's quoting, so a string element
/// comes back quoted.
#[test]
fn the_json_returning_variants_requote() {
    assert_eq!(
        rows("SELECT * FROM json_array_elements('[\"a\",1]')"),
        vec![Some("\"a\"".to_string()), Some("1".to_string())]
    );
    assert_eq!(
        rows("SELECT * FROM jsonb_array_elements('[\"a\",1]')"),
        vec![Some("\"a\"".to_string()), Some("1".to_string())]
    );
}

/// The `_text` variants do not, which is the half a single shared projection
/// would have got wrong.
#[test]
fn the_text_variants_do_not_requote() {
    assert_eq!(
        rows("SELECT * FROM json_array_elements_text('[\"a\",1]')"),
        vec![Some("a".to_string()), Some("1".to_string())]
    );
}

/// A nested element stays itself rather than being quoted into a string.
#[test]
fn a_nested_element_is_not_double_quoted() {
    assert_eq!(
        rows("SELECT * FROM json_array_elements('[{\"a\":1}]')"),
        vec![Some("{\"a\":1}".to_string())]
    );
}

/// `json_object_keys` projects the key column.
#[test]
fn object_keys_projects_the_keys() {
    assert_eq!(
        rows("SELECT * FROM json_object_keys('{\"k\":1,\"j\":2}')"),
        vec![Some("k".to_string()), Some("j".to_string())]
    );
}

/// PostgreSQL's `json_each` exposes exactly `key` and `value`. SQLite's exposes
/// eight columns, so a passthrough silently widened the row: accepted by
/// SQLite, wrong against the source.
///
/// Proven by inserting the result into a two column table, which fails outright
/// if the relation carries more, rather than by reading columns by name, which
/// would ignore the extra ones and pass.
#[test]
fn each_exposes_only_key_and_value() {
    assert_eq!(
        run_translated_with(
            &format!(
                "{FIXTURE}
                 CREATE TABLE pairs (k TEXT, v TEXT);
                 INSERT INTO pairs SELECT * FROM json_each('{{\"a\":1}}');
                 SELECT k || '=' || v FROM pairs;"
            ),
            &Pg2SqliteOptions::default(),
        ),
        vec![Some("a=1".to_string())]
    );
}

/// An argument naming a column is an implicit `LATERAL` in PostgreSQL, and the
/// derived table that supplies the column names cannot see a sibling `FROM`
/// item, so it is refused with the advice `UNNEST` already gives. Verified on
/// SQLite: the correlated form works as a bare passthrough and fails inside a
/// derived table with `no such column`.
#[test]
fn a_correlated_argument_is_refused_with_advice() {
    let error = refuse("SELECT * FROM t, json_array_elements(t.payload)");
    assert!(error.contains("LATERAL"), "the reason must name LATERAL, got: {error}");
    assert!(error.contains("json_each"), "the advice must name json_each, got: {error}");
}

/// The derived table exists to name the output columns, so the alias forms
/// carry the real logic and each one has to keep resolving. PostgreSQL's
/// default names are `value`, `key` and `value`, and the function's own name
/// for `json_object_keys`, all measured on 16. SQLite accepts no column list on
/// a table alias, so an explicit list has to become the projection's aliases.
#[test]
fn the_alias_forms_keep_their_column_names() {
    // A column list renames the element column.
    assert_eq!(
        rows("SELECT e.v FROM json_array_elements('[1,2]') AS e(v)"),
        vec![Some("1".to_string()), Some("2".to_string())]
    );
    // Without one, PostgreSQL's default name resolves through the alias.
    assert_eq!(
        rows("SELECT e.value FROM json_array_elements('[1]') AS e"),
        vec![Some("1".to_string())]
    );
    // Both columns of the two column form resolve.
    assert_eq!(
        rows("SELECT e.key || '=' || e.value FROM json_each('{\"a\":1}') AS e"),
        vec![Some("a=1".to_string())]
    );
    // An implicit alias, with no AS, behaves the same.
    assert_eq!(
        rows("SELECT jae.value FROM json_array_elements('[1]') jae"),
        vec![Some("1".to_string())]
    );
}

/// A call whose arity is not PostgreSQL's is refused, and the message reports
/// the count it saw rather than assuming there were too many.
#[test]
fn a_call_with_the_wrong_arity_is_refused() {
    let error = refuse("SELECT * FROM json_each()");
    assert!(error.contains("0 arguments"), "the count must be reported, got: {error}");
}
