//! Which written form of a name this crate may claim as a PostgreSQL built-in.
//!
//! The translator used to read the last segment of a function or type name,
//! lowercase it, and look that up, which made three different names one:
//!
//! - `"RANDOM"()`, a function the script defines itself and which returns 42 in
//!   PostgreSQL 17.3, came out as the built-in random-fraction rewrite.
//! - `app.random()`, returning 7 in PostgreSQL, came out the same way.
//! - `SELECT app.upper(name)` passed through untouched, and SQLite refuses that
//!   with `near "(": syntax error`, since it has no schema-qualified function
//!   call at all. Even `main.abs(-1)` over a real attached database is a syntax
//!   error.
//!
//! PostgreSQL keeps the capitals of a delimited identifier, so only a spelling
//! that quoting leaves alone can name a catalogue entry. Measured on 17.3:
//! `"random"()` and `pg_catalog.now()` resolve to the built-ins, `"NOW"()` does
//! not exist.
//!
//! Types take the quoting half of that rule and not the prefix half, because an
//! extension may be installed into a named schema, which is why
//! `public.vector` maps to `BLOB` and must keep doing so.

use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};
use rusqlite::Connection;

fn translate(pg: &str, options: &Pg2SqliteOptions) -> Result<Vec<String>, Error> {
    Pg2Sqlite::default().sql(pg).and_then(|loaded| loaded.translate_to_sql(options))
}

fn refusal(pg: &str, options: &Pg2SqliteOptions) -> String {
    match translate(pg, options) {
        Ok(emitted) => panic!("translation should have refused, emitted: {emitted:?}"),
        Err(error) => error.to_string(),
    }
}

/// Runs the single emitted statement and answers its first column as text.
fn run_one(pg: &str, options: &Pg2SqliteOptions) -> String {
    let emitted = translate(pg, options).expect("translation succeeds");
    let [probe] = emitted.as_slice() else {
        panic!("expected one emitted statement, got {emitted:?}");
    };
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    connection
        .query_row(probe, [], |row| row.get::<_, String>(0))
        .unwrap_or_else(|error| panic!("emitted statement failed: {error}\n{probe}"))
}

const OWN_RANDOM: &str = r#"
CREATE FUNCTION "RANDOM"() RETURNS INT AS $$ SELECT 42 $$ LANGUAGE sql;
SELECT "RANDOM"();
"#;

const QUALIFIED_RANDOM: &str = "
CREATE SCHEMA app;
CREATE FUNCTION app.random() RETURNS INT AS $$ SELECT 7 $$ LANGUAGE sql;
SELECT app.random();
";

#[test]
fn a_quoted_name_carrying_a_capital_is_not_the_builtin() {
    let message = refusal(OWN_RANDOM, &Pg2SqliteOptions::default());
    assert!(
        message.contains("RANDOM") && message.contains("with_user_defined_functions"),
        "the refusal should name the function and the way to declare it, got: {message}"
    );
}

#[test]
fn a_schema_qualified_call_is_refused_for_want_of_syntax() {
    let message = refusal(QUALIFIED_RANDOM, &Pg2SqliteOptions::default());
    assert!(
        message.contains("app.random") && message.contains("qualified"),
        "the refusal should name the call and why SQLite cannot carry it, got: {message}"
    );
}

#[test]
fn a_schema_qualified_call_naming_a_sqlite_function_is_refused_too() {
    let message =
        refusal("CREATE SCHEMA app; SELECT app.upper('x') AS v;", &Pg2SqliteOptions::default());
    assert!(
        message.contains("app.upper") && message.contains("qualified"),
        "passing this through emits SQL SQLite cannot parse, got: {message}"
    );
}

#[test]
fn a_schema_qualified_call_stays_refused_when_the_name_is_declared() {
    let options = Pg2SqliteOptions::default().with_user_defined_functions(["random"]);
    let message = refusal(QUALIFIED_RANDOM, &options);
    assert!(
        message.contains("qualified"),
        "SQLite has no syntax for the call however the name is declared, got: {message}"
    );
}

#[test]
fn a_quoted_name_carrying_a_capital_passes_through_once_declared() {
    let options = Pg2SqliteOptions::default().with_user_defined_functions(["random"]);
    let emitted = translate(OWN_RANDOM, &options).expect("a declared name is the caller's word");
    assert_eq!(emitted, [r#"SELECT "RANDOM"()"#]);
}

#[test]
fn a_quoted_name_quoting_leaves_alone_is_still_the_builtin() {
    let value = run_one(r#"SELECT "upper"('abc') AS v;"#, &Pg2SqliteOptions::default());
    assert_eq!(value, "ABC");
}

#[test]
fn the_catalogue_prefix_still_names_the_builtin() {
    let emitted = translate("SELECT pg_catalog.now() AS v;", &Pg2SqliteOptions::default())
        .expect("pg_catalog.now names the built-in");
    assert_eq!(emitted, ["SELECT datetime('now') AS v"]);
}

#[test]
fn a_bare_builtin_is_untouched_by_the_new_rule() {
    let emitted = translate("SELECT now() AS v;", &Pg2SqliteOptions::default())
        .expect("bare now names the built-in");
    assert_eq!(emitted, ["SELECT datetime('now') AS v"]);
}

#[test]
fn a_quoted_type_name_quoting_leaves_alone_still_maps() {
    let emitted = translate(
        r#"CREATE TABLE t (id INT PRIMARY KEY, v "vector"(3));"#,
        &Pg2SqliteOptions::default(),
    )
    .expect(r#""vector" names the pgvector type"#);
    assert!(
        emitted[0].contains("v BLOB"),
        "the column should still store the vector as a blob, got: {emitted:?}"
    );
}

#[test]
fn a_quoted_type_name_carrying_a_capital_is_not_the_extension_type() {
    let message = refusal(
        r#"CREATE TABLE t (id INT PRIMARY KEY, v "Vector"(3));"#,
        &Pg2SqliteOptions::default(),
    );
    assert!(
        message.contains("Unknown PostgreSQL custom type") && message.contains("Vector"),
        "the refusal should name the type as written, got: {message}"
    );
}

#[test]
fn a_prefixed_type_name_still_maps() {
    let emitted = translate(
        "CREATE SCHEMA app; CREATE TABLE t (id INT PRIMARY KEY, v app.geometry);",
        &Pg2SqliteOptions::default(),
    )
    .expect("an extension type may live in a named schema");
    assert!(
        emitted.iter().any(|statement| statement.contains("v BLOB")),
        "a prefixed extension type keeps translating, got: {emitted:?}"
    );
}

#[test]
fn a_quoted_serial_carrying_a_capital_is_not_the_serial_shorthand() {
    let message = refusal(r#"CREATE TABLE t (id "SERIAL");"#, &Pg2SqliteOptions::default());
    assert!(
        message.contains("Unknown PostgreSQL custom type"),
        "PostgreSQL keeps the capitals, so this is not the serial shorthand, got: {message}"
    );
}

#[test]
fn a_prefixed_call_the_script_does_not_define_is_taken_as_the_builtin() {
    let emitted = translate("SELECT public.now() AS v;", &Pg2SqliteOptions::default())
        .expect("nothing defines public.now, so it names the catalogue's now");
    assert_eq!(emitted, ["SELECT datetime('now') AS v"]);
}

#[test]
fn a_prefixed_extension_function_keeps_translating() {
    let emitted = translate(
        "CREATE TABLE t (id TEXT DEFAULT public.gen_random_uuid());",
        &Pg2SqliteOptions::default(),
    )
    .expect("pgcrypto may live in a named schema");
    assert!(
        emitted[0].contains("uuid()"),
        "the default should still generate a uuid, got: {emitted:?}"
    );
}

/// The discrimination rests on what the script defines, and a definition
/// records only its last segment, so a prefixed call to a function the script
/// never defines is still read as the catalogue's. Pinned rather than fixed:
/// nothing in the input says otherwise, and refusing every prefixed call would
/// take `public.gen_random_uuid()` with it.
#[test]
fn a_prefixed_call_to_an_undefined_function_is_still_read_as_the_builtin() {
    let emitted =
        translate("CREATE SCHEMA app; SELECT app.random() AS v;", &Pg2SqliteOptions::default())
            .expect("nothing in the script claims this name");
    assert!(
        emitted[0].contains("random()"),
        "the built-in rewrite is what this answers today, got: {emitted:?}"
    );
}
