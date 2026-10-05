//! What the `json` and `jsonb` operators answer.
//!
//! Two of them were emitted as the arithmetic and text operators they look
//! like: `d - 'a'` subtracted and answered 0 where PostgreSQL removes the
//! key, and `d || '{"b":2}'` concatenated two documents into text that is not
//! JSON at all. The rest here are the narrower ones: a numeric path element,
//! a key carrying a quote, a non-array length, a boolean, and key order.
//!
//! Every expected value is what PostgreSQL 17.3 answers, measured in Docker.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

const DOCUMENT: &str = "CREATE TABLE jd (d jsonb);
     INSERT INTO jd VALUES ('{\"a\":1,\"s\":\"x\"}');";

/// The rows of `SELECT <expression> FROM jd` over that document.
fn answer(expression: &str) -> Vec<Option<String>> {
    run_translated_with(
        &format!("{DOCUMENT} SELECT {expression} FROM jd;"),
        &Pg2SqliteOptions::default(),
    )
}

/// The rows of a whole script's last statement.
fn script(pg: &str) -> Vec<Option<String>> {
    run_translated_with(pg, &Pg2SqliteOptions::default())
}

/// The refusal `pg` earns.
fn refusal(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("expected a refusal")
        .to_string()
}

#[test]
fn removing_a_key_removes_it() {
    // PostgreSQL: {"s": "x"}.
    assert_eq!(answer("d - 'a'"), vec![Some(r#"{"s":"x"}"#.to_string())]);
}

#[test]
fn removing_several_keys_removes_all_of_them() {
    // PostgreSQL: {} for ARRAY['a','s'].
    assert_eq!(answer("d - ARRAY['a','s']"), vec![Some("{}".to_string())]);
}

#[test]
fn removing_an_array_element_by_index_removes_it() {
    // PostgreSQL: [1,3] for '[1,2,3]'::jsonb - 1.
    assert_eq!(script("SELECT '[1,2,3]'::jsonb - 1;"), vec![Some("[1,3]".to_string())]);
}

#[test]
fn removing_a_key_named_by_an_expression_is_refused() {
    // The path shape is decided when the statement is translated, and a
    // column could name either a key or an index.
    let message = refusal(
        "CREATE TABLE jd (d jsonb, k text);
         SELECT d - k FROM jd;",
    );
    assert!(message.contains('-'), "{message}");
    assert!(message.to_lowercase().contains("literal"), "{message}");
}

#[test]
fn merging_two_documents_is_refused() {
    // PostgreSQL answers a shallow merge. SQLite's json_patch is a different
    // operation: it deletes a key whose value is null and replaces an array
    // rather than concatenating it, both measured.
    let message = refusal(&format!("{DOCUMENT} SELECT d || '{{\"b\":2}}'::jsonb FROM jd;"));
    assert!(message.contains("json_patch"), "{message}");
    assert!(message.to_lowercase().contains("null"), "{message}");
}

#[test]
fn a_numeric_path_element_is_refused_rather_than_answered_wrongly() {
    // `#-` answered the document unchanged and extract_path answered NULL,
    // because a numeric element was written as a key rather than an index,
    // and PostgreSQL decides which it is from the container at run time.
    let removal = refusal("SELECT '[1,2,3]'::jsonb #- '{1}';");
    assert!(removal.contains('1'), "{removal}");
    assert!(removal.to_lowercase().contains("array"), "{removal}");
    let extraction = refusal("SELECT jsonb_extract_path('[1,2,3]'::jsonb, '0');");
    assert!(extraction.to_lowercase().contains("array"), "{extraction}");
}

#[test]
fn a_key_carrying_a_quote_is_still_found() {
    // PostgreSQL answers 1 for the key `a"b`; the emitted path had ended the
    // quoted key early and matched nothing.
    assert_eq!(
        script(
            r#"CREATE TABLE jk (d jsonb);
               INSERT INTO jk VALUES ('{"a\"b":1}');
               SELECT jsonb_extract_path(d, 'a"b') FROM jk;"#
        ),
        vec![Some("1".to_string())]
    );
}

#[test]
fn a_json_object_takes_a_key_that_is_not_text() {
    // PostgreSQL coerces the key: {"1" : 2}. The emitted json_object had
    // answered `labels must be TEXT` when the query ran.
    assert_eq!(script("SELECT json_build_object(1, 2);"), vec![Some(r#"{"1":2}"#.to_string())]);
}

#[test]
fn the_length_of_a_non_array_is_not_a_number() {
    // PostgreSQL: ERROR: cannot get array length of a non-array. SQLite
    // cannot raise inside an expression, so the answer is NULL rather than
    // the 0 it used to give.
    assert_eq!(answer("jsonb_array_length(d)"), vec![None]);
    assert_eq!(script("SELECT jsonb_array_length('[1,2,3]'::jsonb);"), vec![Some("3".to_string())]);
}

#[test]
fn a_boolean_becomes_json_true() {
    // PostgreSQL: true. The emitted json_quote had answered 1.
    assert_eq!(script("SELECT to_jsonb(true);"), vec![Some("true".to_string())]);
    assert_eq!(script("SELECT to_jsonb(false);"), vec![Some("false".to_string())]);
}

#[test]
fn an_untyped_argument_to_to_jsonb_is_refused() {
    // PostgreSQL: could not determine polymorphic type because input has
    // type unknown.
    let message = refusal("SELECT to_jsonb('x');");
    assert!(message.to_lowercase().contains("type"), "{message}");
}

#[test]
fn jsonb_keys_come_back_in_canonical_order() {
    // PostgreSQL stores jsonb with its keys sorted, so the keys answer a, m,
    // z where the document was written z, a, m.
    assert_eq!(
        script("SELECT * FROM jsonb_object_keys('{\"z\":1,\"a\":2,\"m\":3}');"),
        vec![Some("a".to_string()), Some("m".to_string()), Some("z".to_string())]
    );
}

#[test]
fn json_keys_keep_document_order() {
    // The non-b spelling preserves the document's order in PostgreSQL too,
    // so it must not be sorted.
    assert_eq!(
        script("SELECT * FROM json_object_keys('{\"z\":1,\"a\":2,\"m\":3}');"),
        vec![Some("z".to_string()), Some("a".to_string()), Some("m".to_string())]
    );
}

#[test]
fn a_value_postgresql_will_not_put_in_a_jsonb_column_is_refused() {
    // PostgreSQL: column "v" is of type jsonb but expression is of type
    // integer.
    let message = refusal(
        "CREATE TABLE ja (v jsonb);
         INSERT INTO ja VALUES (1);",
    );
    assert!(message.to_lowercase().contains("jsonb"), "{message}");

    // And a document carrying a NUL escape, which PostgreSQL refuses for
    // jsonb: unsupported Unicode escape sequence.
    let nul = refusal(
        r#"CREATE TABLE jn (d jsonb);
           INSERT INTO jn VALUES ('{"a":"\u0000"}');"#,
    );
    assert!(nul.contains("0000"), "{nul}");
}

#[test]
fn the_operators_that_already_answered_correctly_still_do() {
    assert_eq!(answer("d -> 'a'"), vec![Some("1".to_string())]);
    assert_eq!(answer("d ->> 's'"), vec![Some("x".to_string())]);
    assert_eq!(answer("d ? 'a'"), vec![Some("1".to_string())]);
    assert_eq!(answer("jsonb_typeof(d)"), vec![Some("object".to_string())]);
    assert_eq!(answer("jsonb_extract_path(d, 'a')"), vec![Some("1".to_string())]);
    assert_eq!(answer("d #- '{a}'"), vec![Some(r#"{"s":"x"}"#.to_string())]);
}
