//! The replica's caller function, reverse translated back into the setting it
//! stands for.
//!
//! A mapping states that a PostgreSQL setting and a SQLite function are the
//! same thing. Going out, the setting becomes the function and the cast over it
//! is dropped, because SQLite is dynamically typed. Coming back, the function
//! has to become the setting again, and the cast has to be written from the
//! type the mapping records: PostgreSQL's `current_setting` answers text, and
//! `uuid = text` is an error there rather than a comparison.
//!
//! Every emitted statement is re-parsed with the PostgreSQL dialect, which is
//! the least this direction owes. The real server checks the same shapes in
//! `tests/gauntlet/reverse.rs`.

use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping},
};
use sql_traits::structs::ParserDB;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

const DDL: &str = "CREATE TABLE projects(id INTEGER PRIMARY KEY, name TEXT);
CREATE TABLE project_members(project_id INTEGER REFERENCES projects(id), user_id TEXT, PRIMARY KEY(project_id, user_id));
CREATE TABLE docs(id INTEGER PRIMARY KEY, project_id INTEGER, title TEXT);";

/// The function the replica registers, which is what the client's own queries
/// name.
const PAIRED_FUNCTION: &str = "app_user_id";

/// The setting the server binds per connection.
const SETTING: &str = "app.user_id";

fn schema() -> ParserDB {
    Pg2Sqlite::default()
        .sql(DDL)
        .expect("the fixture parses")
        .build_schema()
        .expect("the fixture builds a schema")
}

fn reverse(sqlite: &str, options: &Pg2SqliteOptions) -> Result<String, Error> {
    let statements = Pg2Sqlite::default().reverse_sql(sqlite, &schema(), options)?;
    let postgres = statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    Parser::parse_sql(&PostgreSqlDialect {}, &postgres)
        .unwrap_or_else(|error| panic!("emitted `{postgres}` is not PostgreSQL: {error}"));
    Ok(postgres)
}

fn setting_paired() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_setting(SETTING, PAIRED_FUNCTION))
}

#[test]
fn the_paired_function_becomes_the_setting() {
    let postgres = reverse(
        "SELECT * FROM docs WHERE project_id IN \
         (SELECT project_id FROM project_members WHERE user_id = app_user_id())",
        &setting_paired(),
    )
    .expect("the mapping says what the function stands for");

    assert!(
        postgres.contains("current_setting('app.user_id', true)")
            && !postgres.contains(PAIRED_FUNCTION),
        "the paired call becomes the setting, got: {postgres}"
    );
}

#[test]
fn a_recorded_type_becomes_a_cast() {
    let options = Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting(SETTING, PAIRED_FUNCTION).with_pg_type("uuid"),
    );

    let postgres = reverse("SELECT * FROM docs WHERE title = app_user_id()", &options)
        .expect("the mapping records what the setting holds");

    assert!(
        postgres.contains("current_setting('app.user_id', true)::UUID"),
        "the cast the forward direction dropped is written again, got: {postgres}"
    );
}

#[test]
fn a_parameterised_recorded_type_keeps_its_parameters() {
    let options = Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting("app.rate", "app_rate")
            .with_pg_type("numeric(10,2)"),
    );

    let postgres = reverse("SELECT * FROM docs WHERE id = app_rate()", &options)
        .expect("a parameterised type is a type");

    assert!(postgres.contains("::NUMERIC(10,2)"), "precision and scale survive, got: {postgres}");
}

#[test]
fn the_current_user_pattern_becomes_the_bare_keyword() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_user("sqlite_user"));

    let postgres = reverse("SELECT * FROM docs WHERE title = sqlite_user()", &options)
        .expect("the mapping says the function stands for the role");

    assert!(
        postgres.contains("current_user") && !postgres.contains("current_user()"),
        "PostgreSQL refuses `current_user()` with parentheses, so the keyword is bare, got: \
         {postgres}"
    );
}

#[test]
fn one_function_from_both_patterns_becomes_the_setting() {
    let options = Pg2SqliteOptions::default().with_session_user(SETTING, PAIRED_FUNCTION);

    let postgres = reverse("SELECT * FROM docs WHERE title = app_user_id()", &options)
        .expect("both patterns pair with this function, and one of them has to win");

    assert!(
        postgres.contains("current_setting('app.user_id', true)"),
        "the setting the application binds wins over the role the connection opened as, got: \
         {postgres}"
    );
}

#[test]
fn the_paired_function_called_with_arguments_refuses() {
    let error = reverse("SELECT * FROM docs WHERE title = app_user_id(title)", &setting_paired())
        .expect_err("the paired function takes no arguments");

    let message = error.to_string();
    assert!(
        message.contains(PAIRED_FUNCTION) && message.contains(SETTING),
        "the refusal names the function and the setting it pairs with, got: {message}"
    );
}

#[test]
fn the_forward_and_reverse_directions_are_inverses() {
    let options = setting_paired();
    let postgres_source =
        format!("{DDL} SELECT id FROM docs WHERE title = current_setting('{SETTING}');");

    let forward = Pg2Sqlite::default()
        .sql(&postgres_source)
        .expect("the document parses")
        .translate(&options)
        .expect("the setting becomes the paired call");
    let sqlite_query = forward.last().expect("the query is emitted").to_string();
    assert!(sqlite_query.contains("app_user_id()"), "forward emits the paired call");

    let back = reverse(&sqlite_query, &options).expect("and the paired call becomes the setting");
    assert!(
        back.contains("current_setting('app.user_id', true)"),
        "the round trip returns the setting it started from, got: {back}"
    );
}
