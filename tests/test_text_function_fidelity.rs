//! What the text functions and pattern operators answer.
//!
//! `substring('Tom' for 2)` answered `om`, because the length was written
//! where SQLite expects the start position, and `substring(s from 'l+')`
//! answered the whole string, because PostgreSQL's regular-expression form
//! was read as a position too. `LIKE ANY` emitted SQL SQLite cannot run at
//! all. Every expected value is what PostgreSQL 17.3 answers, measured.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::{
    prelude::{ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions},
    warnings::TranslationWarning,
};
use run_translated_helper::run_translated_with;

fn arrays() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json)
}

/// The rows `pg` answers.
fn answer(pg: &str) -> Vec<Option<String>> {
    run_translated_with(pg, &Pg2SqliteOptions::default())
}

/// The rows `pg` answers under the JSON array representation.
fn answer_with_arrays(pg: &str) -> Vec<Option<String>> {
    run_translated_with(pg, &arrays())
}

/// The refusal `pg` earns.
fn refusal_with(pg: &str, options: &Pg2SqliteOptions) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate(options)
        .expect_err("expected a refusal")
        .to_string()
}

/// The refusal `pg` earns under the default options.
fn refusal(pg: &str) -> String {
    refusal_with(pg, &Pg2SqliteOptions::default())
}

/// The warnings `pg` emits under `options`.
fn warnings(pg: &str, options: &Pg2SqliteOptions) -> Vec<TranslationWarning> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_with_report(options)
        .expect("fixture translates")
        .warnings
}

#[test]
fn the_length_only_substring_counts_from_the_start() {
    // PostgreSQL: To. The emitted SUBSTR('Tom', 2) answered om.
    assert_eq!(answer("SELECT substring('Tom' for 2);"), vec![Some("To".to_string())]);
    assert_eq!(answer("SELECT substring('Tom' for 0);"), vec![Some(String::new())]);
}

#[test]
fn a_substring_over_a_pattern_is_refused() {
    // PostgreSQL answers ll for substring('hello' from 'l+'); the emitted
    // SUBSTR read the pattern as a position and answered the whole string.
    let message = refusal("SELECT substring('hello' from 'l+');");
    assert!(message.to_lowercase().contains("regular-expression"), "{message}");
}

#[test]
fn a_negative_substring_length_is_refused() {
    // PostgreSQL: ERROR: negative substring length not allowed. The replica
    // answered an empty string.
    let message = refusal("SELECT substring('abcdef' from 2 for -1);");
    assert!(message.to_lowercase().contains("negative"), "{message}");
}

#[test]
fn the_substring_forms_that_were_right_stay_right() {
    // Measured on both engines: a clamped start, a start past the end, and a
    // multi-byte operand.
    assert_eq!(answer("SELECT substring('abcdef' from -2 for 4);"), vec![Some("a".to_string())]);
    assert_eq!(answer("SELECT substring('abcdef' from 3);"), vec![Some("cdef".to_string())]);
    assert_eq!(answer("SELECT substring('Ünïcödé' from 2 for 2);"), vec![Some("nï".to_string())]);
}

#[test]
fn an_overlay_starting_before_the_first_character_is_refused() {
    // PostgreSQL: ERROR: negative substring length not allowed, for both
    // spellings. The replica answered XYbcdef and XYabcdef.
    for start in ["0", "-1"] {
        let message = refusal(&format!("SELECT overlay('abcdef' placing 'XY' from {start});"));
        assert!(message.to_lowercase().contains("first character"), "{start}: {message}");
    }
    // The positive starts stay as they were, measured on both engines.
    assert_eq!(
        answer("SELECT overlay('abcdef' placing 'XY' from 3 for 2);"),
        vec![Some("abXYef".to_string())]
    );
}

#[test]
fn a_pattern_matched_against_any_element_of_an_array_runs() {
    // PostgreSQL: true, false, true. The emitted LIKE ANY (...) answered
    // `no such function: ANY`.
    assert_eq!(
        answer_with_arrays("SELECT 'a' LIKE ANY (ARRAY['a%', 'b%']);"),
        vec![Some("1".to_string())]
    );
    // `LIKE ALL` is refused: the parser carries the quantifier as a call to a
    // function named ALL, so the emitted statement would answer `no such
    // function: ALL`.
    let all = refusal_with("SELECT 'a' LIKE ALL (ARRAY['a%', 'b%']);", &arrays());
    assert!(all.contains("ALL"), "{all}");
    assert_eq!(
        answer_with_arrays("SELECT 'A' ILIKE ANY (ARRAY['a%']);"),
        vec![Some("1".to_string())]
    );
}

#[test]
fn the_same_works_over_an_array_column() {
    assert_eq!(
        answer_with_arrays(
            "CREATE TABLE t (v text, p text[]);
             INSERT INTO t VALUES ('a', ARRAY['a%', 'b%']);
             SELECT v LIKE ANY (p) FROM t;"
        ),
        vec![Some("1".to_string())]
    );
}

#[test]
fn matching_against_an_array_without_a_representation_is_refused() {
    // Every other array operation is refused without the opt-in, and this
    // one has to be too rather than emitting SQL that cannot run.
    let message = refusal("SELECT 'a' LIKE ANY (ARRAY['a%']);");
    assert!(message.to_lowercase().contains("array"), "{message}");
}

#[test]
fn a_null_character_is_refused() {
    // PostgreSQL: ERROR: null character not permitted. SQLite made a
    // one-byte NUL string that length() then answered 0 for.
    let message = refusal("SELECT chr(0);");
    assert!(message.to_lowercase().contains("null character"), "{message}");
    // Every other code point still translates, measured on both engines.
    assert_eq!(answer("SELECT chr(8364);"), vec![Some("€".to_string())]);
}

#[test]
fn a_null_separator_makes_the_whole_concatenation_null() {
    // PostgreSQL answers NULL whenever the separator is NULL, whatever the
    // values are. The replica answered an empty string for all-NULL values.
    assert_eq!(answer("SELECT concat_ws(NULL, NULL, NULL);"), vec![None]);
    assert_eq!(answer("SELECT concat_ws(NULL, 'a', 'b');"), vec![None]);
    // And the ordinary case is unchanged.
    assert_eq!(
        answer("SELECT concat_ws(',', NULL, 'a', NULL, 'b');"),
        vec![Some("a,b".to_string())]
    );
}

#[test]
fn ascii_only_case_folding_is_reported_when_the_pattern_is_not_a_literal() {
    // A literal pattern carrying a non-ASCII letter is refused already. A
    // pattern held in a column cannot be inspected, and `lower` folds ASCII
    // only, so the caller is told rather than left with a silent mismatch.
    let reported = warnings(
        "CREATE TABLE t (v text, p text);
         SELECT v ILIKE p FROM t;",
        &Pg2SqliteOptions::default(),
    );
    let named: Vec<String> = reported
        .iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LossyDowngrade { construct, reason, .. }
                    if construct == "ILIKE" =>
                {
                    Some(reason.clone())
                }
                _ => None,
            }
        })
        .collect();
    assert_eq!(named.len(), 1, "{reported:?}");
    assert!(named[0].contains("ASCII"), "{}", named[0]);
    assert!(named[0].contains("with_ilike_fold_function"), "{}", named[0]);
}

#[test]
fn naming_a_unicode_aware_fold_function_reports_nothing() {
    let reported = warnings(
        "CREATE TABLE t (v text, p text);
         SELECT v ILIKE p FROM t;",
        &Pg2SqliteOptions::default().with_ilike_fold_function("icu_lower"),
    );
    assert!(
        !reported.iter().any(|warning| format!("{warning:?}").contains("ILIKE")),
        "{reported:?}"
    );
}
