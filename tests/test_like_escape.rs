//! F2: PostgreSQL's LIKE carries an implicit `ESCAPE '\'`, SQLite's does not.
//!
//! Every measurement quoted below was taken on PostgreSQL 17 with
//! `standard_conforming_strings` on (the default), so a backslash written in a
//! string literal is one backslash character in the pattern, and on SQLite
//! 3.46.0 and 3.51.1.
//!
//! Every forward case executes the emitted SQL, because the divergence this
//! guards is a row count, not a keyword.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use run_translated_helper::run_translated_with;

/// Two rows, one holding the literal the pattern escapes and one holding a
/// string only the live wildcard reaches.
const SUBJECTS: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
     INSERT INTO t (id, s) VALUES (1, '100%'), (2, '100abc'), (3, 'a_b'), (4, 'axb'),
                                  (5, 'a\\b'), (6, 'ab');";

fn count(pg_predicate: &str) -> Option<String> {
    let script = format!("{SUBJECTS} SELECT count(*) FROM t WHERE {pg_predicate};");
    run_translated_with(&script, &Pg2SqliteOptions::default()).remove(0)
}

// ---------------------------------------------------------------------------
// Forward: the implicit escape
// ---------------------------------------------------------------------------

/// PostgreSQL: `'100%' LIKE '100\%'` is true and `'100abc' LIKE '100\%'` is
/// false, so the backslash turns the percent into a literal. Without an
/// ESCAPE clause SQLite reads the backslash itself as a literal and the
/// percent stays a wildcard, so both rows matched.
#[test]
fn a_backslash_makes_a_percent_literal() {
    assert_eq!(count(r"s LIKE '100\%'"), Some("1".to_string()));
}

/// The same for the single-character wildcard: PostgreSQL matches `a_b` and
/// not `axb`.
#[test]
fn a_backslash_makes_an_underscore_literal() {
    assert_eq!(count(r"s LIKE 'a\_b'"), Some("1".to_string()));
}

/// The sharpest of the three, because the escape removes a match rather than
/// adding one. PostgreSQL reads `'a\b'` as an escaped `b`, so the pattern is
/// `ab` and the row holding a real backslash does not match, while the row
/// holding `ab` does.
#[test]
fn a_backslash_escapes_an_ordinary_character() {
    assert_eq!(count(r"s LIKE 'a\b'"), Some("1".to_string()));
    assert_eq!(count(r"s LIKE 'a\b' AND s = 'ab'"), Some("1".to_string()));
}

/// NOT LIKE takes the escape too, so it is the complement of the row count
/// above rather than of the unescaped reading.
#[test]
fn not_like_carries_the_escape() {
    assert_eq!(count(r"s NOT LIKE '100\%'"), Some("5".to_string()));
}

/// ILIKE lowers both operands into a LIKE, so it needs the escape appended to
/// the lowered form. PostgreSQL: `'100%' ILIKE '100\%'` is true, `'100abc'`
/// is not.
#[test]
fn ilike_carries_the_escape() {
    assert_eq!(count(r"s ILIKE '100\%'"), Some("1".to_string()));
}

/// NOT ILIKE likewise.
#[test]
fn not_ilike_carries_the_escape() {
    assert_eq!(count(r"s NOT ILIKE 'A\_B'"), Some("5".to_string()));
}

/// A pattern with no backslash reads the same either way, which is what makes
/// the unconditional escape safe. Guards against a fix that escapes the
/// wildcards themselves.
#[test]
fn a_wildcard_still_matches() {
    assert_eq!(count("s LIKE '100%'"), Some("2".to_string()));
}

/// An escape the caller wrote is still the caller's. Nothing here changes for
/// a pattern that already names its escape character.
#[test]
fn an_explicit_escape_is_left_alone() {
    assert_eq!(count("s LIKE 'a#_b' ESCAPE '#'"), Some("1".to_string()));
    assert_eq!(count(r"s LIKE 'a\b' ESCAPE '#'"), Some("1".to_string()));
    assert_eq!(count(r"s LIKE 'a\b' ESCAPE '#' AND s = 'a\b'"), Some("1".to_string()));
}

// ---------------------------------------------------------------------------
// Forward: escaping switched off
// ---------------------------------------------------------------------------

/// PostgreSQL spells "no escape character at all" as `ESCAPE ''`, which is
/// what SQLite's bare LIKE already means, so the clause is dropped rather
/// than forwarded. SQLite refuses the empty spelling outright with `ESCAPE
/// expression must be a single character`, so before this the statement did
/// not even prepare.
#[test]
fn switching_escaping_off_drops_the_clause() {
    assert_eq!(count(r"s LIKE 'a\b' ESCAPE ''"), Some("1".to_string()));
    assert_eq!(count(r"s LIKE 'a\b' ESCAPE '' AND s = 'a\b'"), Some("1".to_string()));
}

/// The same through the ILIKE lowering.
#[test]
fn switching_escaping_off_on_ilike_drops_the_clause() {
    assert_eq!(count(r"s ILIKE 'A\B' ESCAPE ''"), Some("1".to_string()));
    let emitted = Pg2Sqlite::default()
        .sql(r"SELECT 'a' ILIKE 'a' ESCAPE '';")
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .unwrap()
        .join("\n");
    assert!(!emitted.contains("ESCAPE"), "the empty escape must not survive: {emitted}");
}

// ---------------------------------------------------------------------------
// Reverse
// ---------------------------------------------------------------------------

fn reverse(sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql("CREATE TABLE t (a TEXT, b TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let statements =
        translator.reverse_sql(sqlite_sql, &schema, &Pg2SqliteOptions::default()).unwrap();
    let sql = statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    Parser::parse_sql(&PostgreSqlDialect {}, &sql)
        .unwrap_or_else(|error| panic!("reverse output is not PostgreSQL: {error}\n{sql}"));
    sql
}

/// The forward direction now emits the lowered LIKE with the escape attached,
/// so the shape the reverse direction restores ILIKE from is that one. Left
/// unlearned, an ILIKE would survive a round trip as a pair of `lower()`
/// calls.
#[test]
fn a_lowered_like_with_the_default_escape_restores_ilike() {
    assert_eq!(
        reverse(r"SELECT lower(a) LIKE lower(b) ESCAPE '\' FROM t"),
        "SELECT a ILIKE b FROM t"
    );
}

/// The escape is dropped on the way back because a backslash is what
/// PostgreSQL's LIKE uses when no ESCAPE is written, so the two readings are
/// the same and this one is the text the round trip started from.
#[test]
fn ilike_survives_a_round_trip() {
    let options = Pg2SqliteOptions::default();
    let translator = Pg2Sqlite::default().sql("CREATE TABLE t (a TEXT, b TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let forward = Pg2Sqlite::default()
        .sql("CREATE TABLE t (a TEXT, b TEXT); SELECT a ILIKE b FROM t;")
        .unwrap()
        .translate_to_sql(&options)
        .unwrap();
    let query = forward.iter().find(|s| s.starts_with("SELECT")).expect("a translated query");
    let back = translator.reverse_sql(query, &schema, &options).unwrap();
    assert_eq!(back[0].to_string(), "SELECT a ILIKE b FROM t");
}

/// A plain LIKE keeps the escape on the way back, which is the same reading
/// PostgreSQL gives an unadorned LIKE, so a second forward pass is a fixed
/// point.
#[test]
fn a_plain_like_round_trips_to_the_same_reading() {
    let options = Pg2SqliteOptions::default();
    let first = Pg2Sqlite::default()
        .sql("CREATE TABLE t (a TEXT, b TEXT); SELECT a LIKE b FROM t;")
        .unwrap()
        .translate_to_sql(&options)
        .unwrap();
    let query = first.iter().find(|s| s.starts_with("SELECT")).expect("a translated query").clone();
    assert!(query.contains(r"ESCAPE '\'"), "the forward emission carries the escape: {query}");

    let translator = Pg2Sqlite::default().sql("CREATE TABLE t (a TEXT, b TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let back = translator.reverse_sql(&query, &schema, &options).unwrap()[0].to_string();
    let again = Pg2Sqlite::default()
        .sql(&format!("CREATE TABLE t (a TEXT, b TEXT); {back};"))
        .unwrap()
        .translate_to_sql(&options)
        .unwrap();
    assert!(again.contains(&query), "a second pass must not change the emission: {again:?}");
}
