//! The spellings PostgreSQL gives a string literal, and the identifier case
//! rule SQLite does not share.
//!
//! A dollar-quoted literal used to reach SQLite verbatim, where `$$hello$$`
//! reads as a parameter named `$hello$` and the row silently held NULL. An
//! escape string, a tagged dollar quote and a national string reached it too
//! and failed at apply. Every expected value below is what PostgreSQL 17.3
//! answers, measured in Docker.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

/// The value `literal` stores in a `text` column, read back.
fn stored(literal: &str) -> Vec<Option<String>> {
    run_translated_with(
        &format!(
            "CREATE TABLE t (s text);
             INSERT INTO t (s) VALUES ({literal});
             SELECT s FROM t;"
        ),
        &Pg2SqliteOptions::default(),
    )
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
fn a_dollar_quoted_literal_stores_its_text() {
    assert_eq!(stored("$$hello$$"), vec![Some("hello".to_string())]);
}

#[test]
fn a_dollar_quoted_literal_keeps_a_quote_and_a_tag() {
    // PostgreSQL answers a'b for $$a'b$$ and x for $tag$x$tag$.
    assert_eq!(stored("$$a'b$$"), vec![Some("a'b".to_string())]);
    assert_eq!(stored("$tag$x$tag$"), vec![Some("x".to_string())]);
    assert_eq!(stored("$tag$a'b$tag$"), vec![Some("a'b".to_string())]);
}

#[test]
fn an_escape_string_decodes_its_escapes() {
    // Measured: E'a\nb' equals a real newline, E'\x41', E'\101' and
    // E'\u0041' are all A, and E'a\\b' is a backslash between the letters.
    assert_eq!(stored(r"E'a\nb'"), vec![Some("a\nb".to_string())]);
    assert_eq!(stored(r"E'\x41'"), vec![Some("A".to_string())]);
    assert_eq!(stored(r"E'\101'"), vec![Some("A".to_string())]);
    assert_eq!(stored(r"E'\u0041'"), vec![Some("A".to_string())]);
    assert_eq!(stored(r"E'a\\b'"), vec![Some(r"a\b".to_string())]);
    assert_eq!(stored(r"E'a\tb'"), vec![Some("a\tb".to_string())]);
    assert_eq!(stored(r"E'it\'s'"), vec![Some("it's".to_string())]);
}

#[test]
fn a_unicode_escape_string_stores_the_character_it_names() {
    // PostgreSQL answers A for both spellings of the code point.
    assert_eq!(stored(r"U&'\0041'"), vec![Some("A".to_string())]);
    assert_eq!(stored(r"U&'\+000041'"), vec![Some("A".to_string())]);
}

#[test]
fn a_national_string_stores_its_text() {
    // PostgreSQL reads N'abc' as the plain literal abc.
    assert_eq!(stored("N'abc'"), vec![Some("abc".to_string())]);
}

#[test]
fn a_dollar_quoted_literal_survives_a_comparison() {
    assert_eq!(
        run_translated_with(
            "CREATE TABLE t (s text);
             INSERT INTO t (s) VALUES ('hello');
             SELECT s FROM t WHERE s = $$hello$$;",
            &Pg2SqliteOptions::default(),
        ),
        vec![Some("hello".to_string())]
    );
}

#[test]
fn two_columns_differing_only_in_case_are_refused() {
    // PostgreSQL keeps `a` and `"A"` apart; SQLite compares ASCII identifiers
    // case-insensitively and answers `duplicate column name: A` at apply,
    // which is a failure the translation can see coming.
    let message = refusal("CREATE TABLE t (a int, \"A\" int);");
    assert!(message.contains('a') && message.contains('A'), "{message}");
    assert!(message.to_lowercase().contains("case"), "{message}");
}

#[test]
fn columns_that_differ_by_more_than_case_still_translate() {
    assert_eq!(
        run_translated_with(
            "CREATE TABLE t (a int, \"B\" int);
             INSERT INTO t (a, \"B\") VALUES (1, 2);
             SELECT a + \"B\" FROM t;",
            &Pg2SqliteOptions::default(),
        ),
        vec![Some("3".to_string())]
    );
}
