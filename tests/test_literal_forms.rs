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

/// What PostgreSQL answers for `CAST(v AS TEXT)` after a string literal is
/// written into a column of `column_type`.
fn stored_as(column_type: &str, literal: &str) -> Vec<Option<String>> {
    run_translated_with(
        &format!(
            "CREATE TABLE t (v {column_type});
             INSERT INTO t (v) VALUES ({literal});
             SELECT CAST(v AS TEXT) FROM t;"
        ),
        &Pg2SqliteOptions::default(),
    )
}

#[test]
fn a_string_literal_in_a_boolean_column_reads_as_boolin_reads_it() {
    for (literal, expected) in [
        ("'t'", "true"),
        ("' TRUE '", "true"),
        ("'y'", "true"),
        ("'on'", "true"),
        ("'1'", "true"),
        ("'f'", "false"),
        ("'no'", "false"),
        ("'of'", "false"),
        ("'0'", "false"),
    ] {
        assert_eq!(stored_as("boolean", literal), vec![Some(expected.to_string())], "{literal}");
    }
    for literal in ["'o'", "'maybe'", "'10'"] {
        refusal(&format!("CREATE TABLE t (v boolean); INSERT INTO t (v) VALUES ({literal});"));
    }
}

#[test]
fn a_string_literal_in_a_numeric_column_is_scaled_like_a_number() {
    for (literal, expected) in [("'12.5'", "12.50"), ("' -3 '", "-3.00"), ("'.25'", "0.25")] {
        assert_eq!(
            stored_as("numeric(10,2)", literal),
            vec![Some(expected.to_string())],
            "{literal}"
        );
    }
    for literal in ["'NaN'", "'1.2.3'", "'12.505'"] {
        refusal(&format!(
            "CREATE TABLE t (v numeric(10,2)); INSERT INTO t (v) VALUES ({literal});"
        ));
    }
}

#[test]
fn string_literals_in_updates_upserts_and_defaults_read_as_their_column_type() {
    let read = |pg: &str| {
        run_translated_with(
            &format!(
                "CREATE TABLE t (id int PRIMARY KEY, flag boolean DEFAULT 'y', \
                 amount numeric(10,2) DEFAULT '2.5');
                 {pg}
                 SELECT CAST(flag AS TEXT) || ' ' || CAST(amount AS TEXT) FROM t;"
            ),
            &Pg2SqliteOptions::default(),
        )
    };
    assert_eq!(read("INSERT INTO t (id) VALUES (1);"), vec![Some("true 2.50".to_string())]);
    assert_eq!(
        read("INSERT INTO t (id) VALUES (1); UPDATE t SET flag = 'off', amount = '7.5';"),
        vec![Some("false 7.50".to_string())]
    );
    assert_eq!(
        read(
            "INSERT INTO t (id) VALUES (1);
             INSERT INTO t (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET flag = 'n', amount = '-1';"
        ),
        vec![Some("false -1.00".to_string())]
    );
}
