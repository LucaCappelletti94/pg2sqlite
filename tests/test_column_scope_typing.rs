//! Which table a column reference resolves to before its declared type is
//! read.
//!
//! The translator used to drop the qualifier and scan every table in the schema
//! for a column of that name, insisting they all agree. Three measured
//! consequences, one per direction plus a false alarm:
//!
//! - `to_json(a.payload)` with `a.payload JSON` and `b.payload TEXT` declared
//!   refused as ambiguous, advising the caller to qualify a reference that was
//!   already qualified.
//! - With only `b.payload JSON` declared, `to_json(a.payload)` over an
//!   undeclared `a` emitted `json(a.payload)`, which fails at runtime with
//!   `malformed JSON` over real text. An unrelated table decided the type.
//! - Reading `json_type(b.payload)` back over a `jsonb` column gave
//!   `json_typeof`, which PostgreSQL rejects with `function json_typeof(jsonb)
//!   does not exist`. `jsonb_typeof` answers `object`.
//!
//! Resolution now runs in the scope of the enclosing query, or of the table a
//! definition belongs to, and a reference the scope cannot answer is refused
//! rather than guessed.

use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};
use rusqlite::Connection;

fn translate(pg: &str, options: &Pg2SqliteOptions) -> Result<Vec<String>, Error> {
    Pg2Sqlite::default().sql(pg).and_then(|loaded| loaded.translate_to_sql(options))
}

fn refusal(pg: &str) -> String {
    match translate(pg, &Pg2SqliteOptions::default()) {
        Ok(emitted) => panic!("translation should have refused, emitted: {emitted:?}"),
        Err(error) => error.to_string(),
    }
}

/// Applies every emitted statement but the last, then answers the last one's
/// first column.
fn run(pg: &str) -> String {
    let mut emitted =
        translate(pg, &Pg2SqliteOptions::default()).expect("translation should succeed");
    let probe = emitted.pop().expect("the script emits at least one statement");
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &emitted {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }
    connection
        .query_row(&probe, [], |row| row.get::<_, String>(0))
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"))
}

fn reverse(sqlite: &str, ddl: &str) -> Result<String, Error> {
    let translator = Pg2Sqlite::default().sql(ddl)?;
    let schema = translator.build_schema()?;
    let statements = translator.reverse_sql(sqlite, &schema, &Pg2SqliteOptions::default())?;
    Ok(statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
}

#[test]
fn a_qualified_reference_reads_its_own_table() {
    let value = run(r#"
        CREATE TABLE a (payload JSON);
        CREATE TABLE b (payload TEXT);
        INSERT INTO a VALUES ('{"k":1}');
        SELECT to_json(a.payload) FROM a;
    "#);
    assert_eq!(
        value, r#"{"k":1}"#,
        "a JSON column is read as a document, not quoted into a string"
    );
}

#[test]
fn a_qualified_reference_to_a_text_column_quotes_it() {
    let value = run(r#"
        CREATE TABLE a (payload TEXT);
        CREATE TABLE b (payload JSON);
        INSERT INTO a VALUES ('hi');
        SELECT to_json(a.payload) FROM a;
    "#);
    assert_eq!(value, r#""hi""#, "a text column is quoted into a JSON string");
}

#[test]
fn a_recreated_view_uses_each_definition_in_its_lifetime() {
    let value = run(r#"
        CREATE TABLE docs (payload JSON);
        CREATE TABLE results (value TEXT);
        INSERT INTO docs VALUES ('{"k":1}');
        CREATE VIEW v AS SELECT payload FROM docs;
        INSERT INTO results SELECT to_json(v.payload) FROM v;
        DROP VIEW v;
        CREATE VIEW v AS SELECT CAST(payload AS TEXT) AS payload FROM docs;
        INSERT INTO results SELECT to_json(v.payload) FROM v;
        SELECT group_concat(value, '|') FROM results;
    "#);
    assert_eq!(value, r#"{"k":1}|"{\"k\":1}""#);
}

#[test]
fn an_unrelated_declaration_no_longer_decides_the_type() {
    let message = refusal("CREATE TABLE b (payload JSON); SELECT to_json(a.payload) FROM a;");
    assert!(
        message.contains("a.payload") || message.contains("payload"),
        "the refusal should name the reference it cannot resolve, got: {message}"
    );
}

#[test]
fn a_bare_reference_two_relations_expose_is_refused() {
    let message = refusal(
        "CREATE TABLE a (id INT, payload JSON);
         CREATE TABLE b (id INT, payload JSON);
         SELECT to_json(payload) FROM a JOIN b ON a.id = b.id;",
    );
    assert!(
        message.to_lowercase().contains("ambiguous") || message.contains("payload"),
        "an unqualified name two relations expose is ambiguous, got: {message}"
    );
}

#[test]
fn an_alias_resolves_to_the_table_it_names() {
    let value = run(r#"
        CREATE TABLE a (payload JSON);
        CREATE TABLE b (payload TEXT);
        INSERT INTO a VALUES ('{"k":2}');
        SELECT to_json(t.payload) FROM a AS t;
    "#);
    assert_eq!(value, r#"{"k":2}"#, "an alias names its own table's column");
}

#[test]
fn a_jsonb_column_reverses_to_the_jsonb_function() {
    let emitted = reverse(
        "SELECT json_type(b.payload) FROM b",
        "CREATE TABLE a (payload JSON); CREATE TABLE b (payload JSONB);",
    )
    .expect("reverse translation should succeed");
    assert_eq!(emitted, "SELECT jsonb_typeof(b.payload) FROM b");
}

#[test]
fn a_json_column_reverses_to_the_json_function() {
    let emitted = reverse(
        "SELECT json_type(a.payload) FROM a",
        "CREATE TABLE a (payload JSON); CREATE TABLE b (payload JSONB);",
    )
    .expect("reverse translation should succeed");
    assert_eq!(emitted, "SELECT json_typeof(a.payload) FROM a");
}

/// A constraint check has no query around it, so its columns come from the
/// table being defined. The scaled-integer rewrite for `NUMERIC` is what proves
/// the type was read: `10.5` becomes `1050` at scale two.
#[test]
fn a_constraint_check_resolves_its_own_table() {
    let emitted = translate(
        "CREATE TABLE p (id INT PRIMARY KEY, amount NUMERIC(10, 2), CHECK (amount > 10.5));",
        &Pg2SqliteOptions::default(),
    )
    .expect("a constraint check over the defined table translates");
    assert!(
        emitted[0].contains("1050"),
        "the literal should be scaled into minor units, got: {emitted:?}"
    );
}

/// A policy condition is also outside any query, and a second table carrying
/// the same column name with a different type must not reach it.
#[test]
fn a_policy_condition_resolves_its_own_table() {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let emitted = translate(
        "CREATE TABLE docs (id INT PRIMARY KEY, owner TEXT NOT NULL, amount NUMERIC(10, 2));
         CREATE TABLE other (amount TEXT);
         ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
         CREATE POLICY docs_owner ON docs USING (amount > 10.5);",
        &options,
    )
    .expect("a policy condition over the defined table translates");
    assert!(
        emitted.iter().any(|statement| statement.contains("1050")),
        "the policy literal should be scaled into minor units, got: {emitted:?}"
    );
}
