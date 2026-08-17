//! PostgreSQL's `#>` and `#>>`, read into SQLite's arrow chains.
//!
//! Both were refused outright, whatever the path, while the crate passed `->`
//! and `->>` chains through untouched, so the refusal declined a rewrite the
//! crate was equipped to perform. `x #> '{a,b}'` is `x -> 'a' -> 'b'`, and
//! `#>>` takes `->>` on the last hop.
//!
//! Every expectation here is a value PostgreSQL 18.4 answered, not a shape:
//! the sharp edges are in the path literal, and a reader that fetched the
//! wrong key would pass any shape assertion.
//!
//! The one genuinely subtle case is a numeric element. PostgreSQL decides
//! key-versus-index by the runtime value: `'{"0":"x"}'::jsonb #> '{0}'`
//! answers `"x"` and `'[1,2]'::jsonb #> '{0}'` answers `1`. SQLite's integer
//! arrow indexes arrays only and its string arrow reads keys only, each
//! answering NULL on the other shape, measured. So a numeric element becomes
//! `COALESCE(x -> 0, x -> '0')`, which reproduces PostgreSQL's runtime
//! decision because each arm is NULL exactly where the other applies.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

const DDL: &str = "CREATE TABLE t (id INT PRIMARY KEY, payload JSONB, arr JSONB, enc TEXT);";

fn forward(postgres: &str) -> Result<String, pg2sqlite::errors::Error> {
    let statements = Pg2Sqlite::default()
        .sql(&format!("{DDL}{postgres};"))?
        .translate_to_sql(&Pg2SqliteOptions::default())?;
    Ok(statements.last().cloned().unwrap_or_default())
}

/// Runs a translated expression over the fixture row and answers the one value
/// rendered as text, `None` for SQL NULL.
///
/// Rendered, because SQLite's `->>` answers a typed value where PostgreSQL's
/// `#>>` answers text: `->> 'b'` on `{"b":2}` is INTEGER 2 there and text `2`
/// here. That is the typing the plain `->>` passthrough already has, so the
/// comparison is on the rendering.
fn value_of(sqlite: &str) -> Option<String> {
    let connection = rusqlite::Connection::open_in_memory().expect("open");
    connection
        .execute_batch(
            r#"CREATE TABLE t (id INTEGER PRIMARY KEY, payload TEXT, arr TEXT, enc TEXT);
               INSERT INTO t VALUES (1,
                 '{"a":{"b":2},"a,b":3,"a b":4,"0":"x","1.5":"half"}',
                 '[10,20,30]',
                 'hex');"#,
        )
        .expect("fixture");
    connection
        .query_row(sqlite, [], |row| row.get::<_, rusqlite::types::Value>(0))
        .map(|value| {
            match value {
                rusqlite::types::Value::Null => None,
                rusqlite::types::Value::Integer(n) => Some(n.to_string()),
                rusqlite::types::Value::Real(r) => Some(r.to_string()),
                rusqlite::types::Value::Text(t) => Some(t),
                rusqlite::types::Value::Blob(_) => Some("<blob>".to_string()),
            }
        })
        .expect("one row")
}

/// Translate, then execute, then compare with what PostgreSQL 18.4 answered
/// for the same document, recorded in the call.
#[track_caller]
fn assert_answers(postgres: &str, expected: Option<&str>) {
    let emitted =
        forward(postgres).unwrap_or_else(|error| panic!("`{postgres}` should translate: {error}"));
    assert_eq!(value_of(&emitted).as_deref(), expected, "`{postgres}` emitted `{emitted}`");
}

// ---------- values, measured against PostgreSQL 18.4 ----------

#[test]
fn a_single_key_reads_the_value() {
    // PG: '{"a":{"b":2}}'::jsonb #> '{a}' answers {"b": 2}; the arrow answers
    // SQLite's compact rendering. That whitespace difference is the one the
    // existing `->` passthrough already carries for composite values, so the
    // assertion uses a scalar leaf instead.
    assert_answers("SELECT payload #> '{a,b}' FROM t", Some("2"));
}

#[test]
fn the_text_form_takes_the_last_hop_only() {
    assert_answers("SELECT payload #>> '{a,b}' FROM t", Some("2"));
    // PG answers x for '{"0":"x"}' at the key, unquoted by #>>.
    assert_answers("SELECT payload #>> '{0}' FROM t", Some("x"));
}

#[test]
fn an_index_reads_the_array() {
    // PG: '[10,20,30]'::jsonb #> '{0}' answers 10.
    assert_answers("SELECT arr #> '{0}' FROM t", Some("10"));
    // PG: "01" converts to index 1.
    assert_answers("SELECT arr #> '{01}' FROM t", Some("20"));
    // PG: a negative index counts from the end, and so does SQLite's, measured.
    assert_answers("SELECT arr #> '{-1}' FROM t", Some("30"));
}

#[test]
fn a_numeric_element_reads_an_object_key_too() {
    // PG: '{"0":"x"}'::jsonb #> '{0}' answers "x". The integer arrow alone
    // answers NULL here, which is what the COALESCE exists for.
    assert_answers("SELECT payload #> '{0}' FROM t", Some("\"x\""));
    // A non-integer numeric is a key and nothing else: PG answers "half" for
    // the key and NULL on an array.
    assert_answers("SELECT payload #> '{1.5}' FROM t", Some("\"half\""));
    assert_answers("SELECT arr #> '{1.5}' FROM t", None);
}

#[test]
fn a_quoted_element_keeps_its_comma_and_space() {
    // PG: quoting is literal syntax, the element is the text inside.
    assert_answers("SELECT payload #> '{\"a,b\"}' FROM t", Some("3"));
    assert_answers("SELECT payload #> '{a b}' FROM t", Some("4"));
    assert_answers("SELECT payload #> '{\"a b\"}' FROM t", Some("4"));
}

#[test]
fn an_absent_path_answers_null() {
    assert_answers("SELECT payload #> '{zz}' FROM t", None);
    assert_answers("SELECT payload #>> '{a,zz}' FROM t", None);
}

#[test]
fn the_empty_path_is_the_document() {
    // PG: x #> '{}' answers x itself, and #>> '{}' its text. The text of a
    // scalar is unquoted on both engines, measured; a composite's text differs
    // in whitespace exactly as the existing ->> passthrough does.
    assert_answers("SELECT payload #> '{}' -> 'a' -> 'b' FROM t", Some("2"));
    assert_answers("SELECT payload #> '{a,b}' #>> '{}' FROM t", Some("2"));
}

// ---------- refusals ----------

#[test]
fn a_computed_path_is_refused_rather_than_guessed() {
    let error = forward("SELECT payload #> enc FROM t")
        .expect_err("the path is not knowable at translation time");
    assert!(error.to_string().contains("literal"), "got: {error}");
}

#[test]
fn a_null_element_is_refused() {
    // PG parses '{NULL}' as a path containing a NULL element and answers NULL.
    // Reproducing that would mean emitting a comparison with NULL that always
    // misses, silently; naming it is worth more.
    let error = forward("SELECT payload #> '{NULL}' FROM t").expect_err("a NULL element");
    assert!(error.to_string().contains("NULL"), "got: {error}");
}

#[test]
fn a_malformed_literal_is_refused() {
    for path in ["'{\"unclosed}'", "'{a,}'", "'no braces'"] {
        let error = forward(&format!("SELECT payload #> {path} FROM t"))
            .expect_err("PostgreSQL would refuse this literal too");
        assert!(
            error.to_string().contains("path"),
            "the refusal for {path} should say what is wrong, got: {error}"
        );
    }
}

// ---------- the round trip this closes ----------

/// The existence operators lower onto `json_type(x, path)`, whose reverse is
/// `jsonb_typeof(x #> '{a}')`, which this arm is what makes translatable back.
#[test]
fn the_existence_operators_survive_the_round_trip() {
    let schema = Pg2Sqlite::default()
        .sql(DDL)
        .expect("fixture parses")
        .build_schema()
        .expect("fixture builds");
    for postgres in [
        "SELECT payload ? 'a' FROM t",
        "SELECT payload ?| ARRAY['a', 'zz'] FROM t",
        "SELECT payload ?& ARRAY['a', '0'] FROM t",
    ] {
        let emitted = forward(postgres).expect("the operator lowers");
        let back = Pg2Sqlite::default()
            .reverse_sql(&emitted, &schema, &Pg2SqliteOptions::default())
            .expect("what the crate emitted, it can read back")
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        let again = forward(&back)
            .unwrap_or_else(|error| panic!("`{back}` should translate back: {error}"));
        assert_eq!(
            value_of(&again),
            value_of(&emitted),
            "the round trip changed the answer for `{postgres}`"
        );
    }
}
