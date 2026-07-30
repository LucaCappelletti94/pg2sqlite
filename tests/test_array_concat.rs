//! `||` on arrays.
//!
//! The operator is overloaded in PostgreSQL: text concatenation, array with
//! array, array with element, and element with array. Under the JSON array
//! representation an array column is TEXT holding a JSON array, so passing `||`
//! through concatenates the two documents as strings and
//! `json_array(1,2) || json_array(3,4)` is the text `[1,2][3,4]`.
//!
//! Measured on PostgreSQL 16 over `(1, {1,2}, {3,4})`, `(2, {}, {5})`, and
//! `(3, NULL, {6})`: `a || b` is `{1,2,3,4}`, `{5}`, and `{6}`, so a NULL
//! operand behaves as an empty array rather than poisoning the result. `a || 9`
//! is `{1,2,9}` and `0 || a` is `{0,1,2}`.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::{
    prelude::{ArrayRepresentation, Pg2SqliteOptions},
    traits::TranslationOptions,
};
use run_translated_helper::run_translated_with;

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, a INT[], b INT[], s TEXT, u TEXT);
     INSERT INTO t VALUES (1, ARRAY[1, 2], ARRAY[3, 4], 'ab', 'cd'),
                          (2, ARRAY[]::INT[], ARRAY[5], '', ''),
                          (3, NULL, ARRAY[6], NULL, NULL);";

fn json_arrays() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json)
}

fn evaluate(expression: &str, id: u8) -> Option<String> {
    run_translated_with(
        &format!("{TABLE} SELECT {expression} FROM t WHERE id = {id};"),
        &json_arrays(),
    )
    .into_iter()
    .next()
    .expect("one row")
}

#[test]
fn two_array_columns_concatenate_as_arrays() {
    assert_eq!(evaluate("a || b", 1), Some("[1,2,3,4]".to_string()));
}

/// The elements stay integers rather than becoming strings, which is what the
/// item asks to confirm.
#[test]
fn the_elements_keep_their_type() {
    let rows = run_translated_with(
        &format!("{TABLE} SELECT json_type(a || b, '$[0]') FROM t WHERE id = 1;"),
        &json_arrays(),
    );
    assert_eq!(rows, vec![Some("integer".to_string())]);
}

/// An empty array contributes nothing, and a NULL one behaves the same way in
/// PostgreSQL rather than making the whole result NULL.
#[test]
fn an_empty_or_null_operand_contributes_nothing() {
    assert_eq!(evaluate("a || b", 2), Some("[5]".to_string()));
    assert_eq!(evaluate("a || b", 3), Some("[6]".to_string()));
}

/// PostgreSQL appends a lone element on either side, which is the same silent
/// wrongness when it falls through to text concatenation.
#[test]
fn an_element_is_appended_or_prepended() {
    assert_eq!(evaluate("a || 9", 1), Some("[1,2,9]".to_string()));
    assert_eq!(evaluate("0 || a", 1), Some("[0,1,2]".to_string()));
}

/// Text concatenation must be left alone. This is what stops the rewrite from
/// keying on the operator rather than on the operands.
#[test]
fn text_concatenation_is_untouched() {
    assert_eq!(evaluate("s || u", 1), Some("abcd".to_string()));
    assert_eq!(evaluate("s || 'z'", 1), Some("abz".to_string()));
}
