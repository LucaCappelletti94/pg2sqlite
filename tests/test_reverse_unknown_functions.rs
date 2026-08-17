//! What the reverse direction does with a name it does not recognise.
//!
//! The forward direction refuses an unknown function, because emitting one
//! produces SQL that fails at run time with `no such function` long after
//! translation reported success. Coming back, the same reasoning holds against
//! PostgreSQL, so the catch-all refuses too. Three things earn a passthrough:
//! this crate's inventory says both engines answer the name the same way, the
//! caller declared it, or an arm translates it into something PostgreSQL has.
//!
//! The inventory's claim that PostgreSQL has a name is checked against a real
//! server in `tests/gauntlet/reverse.rs`. What is checked here is the rule.

use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};
use sql_traits::structs::ParserDB;

fn schema() -> ParserDB {
    Pg2Sqlite::default()
        .sql("CREATE TABLE t (id INT PRIMARY KEY, s TEXT, n INT, r REAL, ts TIMESTAMP);")
        .expect("the fixture parses")
        .build_schema()
        .expect("the fixture builds a schema")
}

fn reverse_with(sqlite: &str, options: &Pg2SqliteOptions) -> Result<String, Error> {
    Pg2Sqlite::default()
        .reverse_sql(sqlite, &schema(), options)
        .map(|statements| statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
}

fn reverse(sqlite: &str) -> Result<String, Error> {
    reverse_with(sqlite, &Pg2SqliteOptions::default())
}

#[test]
fn a_name_neither_engine_shares_refuses() {
    let error = reverse("SELECT levenshtein(s, 'x') FROM t")
        .expect_err("nothing says PostgreSQL has this name");

    let message = error.to_string();
    assert!(
        message.contains("levenshtein")
            && message.contains("function levenshtein() does not exist")
            && message.contains("with_user_defined_functions"),
        "the refusal names what PostgreSQL would answer and how to declare it, got: {message}"
    );
}

#[test]
fn a_declared_name_passes_through() {
    let options = Pg2SqliteOptions::default().with_user_defined_functions(["levenshtein"]);
    let postgres = reverse_with("SELECT levenshtein(s, 'x') FROM t", &options)
        .expect("the caller says PostgreSQL has it");

    assert!(postgres.contains("levenshtein(s, 'x')"), "got: {postgres}");
}

#[test]
fn a_shared_name_passes_through() {
    for sqlite in [
        "SELECT abs(n) FROM t",
        "SELECT length(s) FROM t",
        "SELECT upper(s) FROM t",
        "SELECT coalesce(s, 'x') FROM t",
        "SELECT row_number() OVER (ORDER BY id) FROM t",
        "SELECT sqrt(r) FROM t",
        "SELECT pow(r, 2) FROM t",
    ] {
        let postgres =
            reverse(sqlite).unwrap_or_else(|error| panic!("`{sqlite}` should reverse: {error}"));
        assert!(!postgres.is_empty());
    }
}

#[test]
fn a_sqlite_only_name_refuses_with_its_own_reason() {
    for (sqlite, fragment) in [
        ("SELECT sqlite_source_id() FROM t", "names the SQLite build"),
        ("SELECT glob('a*', s) FROM t", "LIKE or a regular expression"),
        ("SELECT likely(n) FROM t", "planner hint"),
        ("SELECT zeroblob(4) FROM t", "repeat("),
        ("SELECT jsonb_extract(s, '$.a') FROM t", "binary encoding"),
        ("SELECT format('%d', n) FROM t", "printf"),
        ("SELECT log2(r) FROM t", "log(2, x)"),
        ("SELECT soundex(s) FROM t", "fuzzystrmatch"),
    ] {
        let error =
            reverse(sqlite).expect_err(&format!("`{sqlite}` has no PostgreSQL counterpart"));
        let message = error.to_string();
        assert!(
            message.contains(fragment),
            "the refusal for `{sqlite}` should say why, expected {fragment:?}, got: {message}"
        );
    }
}

/// PostgreSQL has both names, so the audit's generic refusal was wrong about
/// them. `date(ts)` runs there unqualified and `time(ts)` does not, `time`
/// being a type name, so both are written as the cast the one-argument form
/// means.
#[test]
fn the_date_and_time_parts_become_casts() {
    for (sqlite, expected) in
        [("SELECT date(ts) FROM t", "::DATE"), ("SELECT time(ts) FROM t", "::TIME")]
    {
        let postgres =
            reverse(sqlite).unwrap_or_else(|error| panic!("`{sqlite}` should reverse: {error}"));
        assert!(postgres.contains(expected), "expected {expected} in: {postgres}");
    }
}

/// SQLite answers the current date and time for the argument-less spellings,
/// which is what the two keywords say in PostgreSQL.
#[test]
fn the_argument_less_date_and_time_become_keywords() {
    for (sqlite, expected) in
        [("SELECT date() FROM t", "current_date"), ("SELECT time() FROM t", "current_time")]
    {
        let postgres =
            reverse(sqlite).unwrap_or_else(|error| panic!("`{sqlite}` should reverse: {error}"));
        assert!(
            postgres.contains(expected) && !postgres.contains(&format!("{expected}(")),
            "the keyword is bare, got: {postgres}"
        );
    }
}

#[test]
fn a_date_or_time_modifier_refuses() {
    for sqlite in ["SELECT date(ts, '+1 day') FROM t", "SELECT time(ts, 'utc') FROM t"] {
        let error = reverse(sqlite).expect_err("a modifier has no PostgreSQL counterpart");
        assert!(
            error.to_string().contains("modifier"),
            "the refusal names the modifier, got: {error}"
        );
    }
}

/// The same document walked as rows rather than read as a value. PostgreSQL's
/// `json_each` answers two columns where SQLite's answers eight, and refuses an
/// array outright, so passing the name through emitted something else entirely.
#[test]
fn a_sqlite_only_row_source_refuses() {
    for (sqlite, fragment) in [
        ("SELECT value FROM json_each(s)", "json_array_elements"),
        ("SELECT value FROM json_tree(s)", "recursively"),
    ] {
        let error = reverse(sqlite).expect_err("the two engines do not answer the same rows");
        assert!(
            error.to_string().contains(fragment),
            "the refusal should say what PostgreSQL has, expected {fragment:?}, got: {error}"
        );
    }
}

/// A row source PostgreSQL does have keeps working, so the refusal above is
/// about the two names rather than about calls in the `FROM` position.
#[test]
fn a_shared_row_source_still_passes_through() {
    let postgres =
        reverse("SELECT value FROM generate_series(1, 2)").expect("both engines generate series");
    assert!(postgres.contains("generate_series"), "got: {postgres}");
}

/// Going out, a geometry name passes through when SQLiteGIS is declared. Coming
/// back it does too, so the option means the same thing in both directions.
#[test]
fn a_geometry_name_passes_through_when_sqlitegis_is_declared() {
    let sqlite = "SELECT ST_Distance(s, s) FROM t";

    let refused = reverse(sqlite).expect_err("without the option nothing vouches for the name");
    assert!(refused.to_string().contains("st_distance"), "got: {refused}");

    let options = Pg2SqliteOptions::default().with_sqlitegis_enabled();
    let postgres = reverse_with(sqlite, &options).expect("the option says both sides have PostGIS");
    assert!(postgres.contains("ST_Distance"), "got: {postgres}");
}
