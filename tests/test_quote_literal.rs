//! `quote_literal` and `quote_nullable`, which differ only on NULL.
//!
//! Measured on PostgreSQL 16:
//!
//! | argument | `quote_literal` | `quote_nullable` |
//! |---|---|---|
//! | `NULL` | NULL | the four characters `NULL` |
//! | `a'b` | `'a''b'` | `'a''b'` |
//! | `42` | `'42'` | `'42'` |
//!
//! Measured on SQLite 3.51.1: `quote(NULL)` is the text `NULL`, which is the
//! `quote_nullable` answer, and `quote(42)` is a bare `42`, which is neither,
//! since PostgreSQL casts to text before quoting.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::Pg2SqliteOptions;
use run_translated_helper::run_translated_with;

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT, n INT);
     INSERT INTO t VALUES (1, 'a''b', 42), (2, NULL, NULL);";

fn evaluate(expression: &str, id: u8) -> Option<String> {
    run_translated_with(
        &format!("{TABLE} SELECT {expression} FROM t WHERE id = {id};"),
        &Pg2SqliteOptions::default(),
    )
    .into_iter()
    .next()
    .expect("one row")
}

/// The one difference between the two functions.
#[test]
fn only_quote_nullable_spells_out_null() {
    assert_eq!(evaluate("quote_literal(s)", 2), None);
    assert_eq!(evaluate("quote_nullable(s)", 2), Some("NULL".to_string()));
}

#[test]
fn both_quote_a_present_string_the_same_way() {
    assert_eq!(evaluate("quote_literal(s)", 1), Some("'a''b'".to_string()));
    assert_eq!(evaluate("quote_nullable(s)", 1), Some("'a''b'".to_string()));
}

/// PostgreSQL casts to text first, so a number comes back quoted. SQLite's
/// `quote` renders it as a bare numeric literal instead, which is a different
/// piece of SQL for a function whose whole purpose is building SQL.
#[test]
fn a_number_comes_back_quoted() {
    assert_eq!(evaluate("quote_literal(n)", 1), Some("'42'".to_string()));
    assert_eq!(evaluate("quote_nullable(n)", 1), Some("'42'".to_string()));
}

/// The string `NULL` is not the NULL value, and `quote_literal` must not
/// confuse the two. This guards the shape of the fix, which recognises the
/// unquoted word `NULL` that `quote` produces for a NULL argument, and the
/// quoted `'NULL'` it produces for this one is six characters and different.
#[test]
fn the_word_null_as_a_value_is_still_quoted() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
         INSERT INTO t VALUES (1, 'NULL');
         SELECT quote_literal(s) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("'NULL'".to_string())]);
}
