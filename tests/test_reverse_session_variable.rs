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

/// The setting that holds the keys a caller holds, joined by a comma.
const SET_SETTING: &str = "app.subjects";

/// The function the replica answers the joined keys with.
const SET_FUNCTION: &str = "current_app_subjects";

fn set_paired() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting(SET_SETTING, SET_FUNCTION).holding_set(','),
    )
}

/// The predicate the forward direction emits for `user_id =
/// ANY(string_to_array(current_setting('app.subjects', true), ','))`, which is
/// the one spelling of a membership test the reverse direction reads.
const MEMBERSHIP: &str = "CASE WHEN current_app_subjects() IS NOT NULL THEN \
    current_app_subjects() <> '' AND instr(user_id, ',') = 0 AND \
    instr(',' || current_app_subjects() || ',', ',' || user_id || ',') > 0 END";

const SET_MEMBERSHIP: &str =
    "user_id = ANY(string_to_array(current_setting('app.subjects', true), ','))";

#[test]
fn the_membership_shape_over_a_set_valued_setting_becomes_any_over_the_split() {
    let postgres = reverse(
        &format!(
            "SELECT * FROM docs WHERE project_id IN \
             (SELECT project_id FROM project_members WHERE {MEMBERSHIP})"
        ),
        &set_paired(),
    )
    .expect("the mapping declares the set the shape tests membership of");

    assert_eq!(
        postgres,
        format!(
            "SELECT * FROM docs WHERE project_id IN \
             (SELECT project_id FROM project_members WHERE {SET_MEMBERSHIP})"
        )
    );
}

/// A strict mapping keeps its one-argument spelling inside the split, since
/// `current_setting(name, true)` there would make the test tolerate an unset
/// setting the scalar reading raises on.
#[test]
fn a_strict_set_valued_setting_keeps_the_strict_spelling_inside_the_split() {
    let options = Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting_strict(SET_SETTING, SET_FUNCTION).holding_set(','),
    );
    let postgres = reverse(&format!("SELECT * FROM project_members WHERE {MEMBERSHIP}"), &options)
        .expect("the strict mapping declares the set");

    assert_eq!(
        postgres,
        "SELECT * FROM project_members WHERE \
         user_id = ANY(string_to_array(current_setting('app.subjects'), ','))"
    );
}

/// Parentheses or a cast to text over the paired function change nothing,
/// since the setting answers text, so the shape still reads as the test.
#[test]
fn parentheses_or_a_text_cast_over_the_paired_function_keep_the_membership_reading() {
    for spelling in ["CAST(current_app_subjects() AS TEXT)", "(current_app_subjects())"] {
        let membership = MEMBERSHIP.replace("current_app_subjects()", spelling);
        let postgres =
            reverse(&format!("SELECT * FROM project_members WHERE {membership}"), &set_paired())
                .expect("the wrapper is a no-op over text");

        assert_eq!(postgres, format!("SELECT * FROM project_members WHERE {SET_MEMBERSHIP}"));
    }
}

/// A pairing states what a name means here, ahead of the inventory that
/// would otherwise read the name as volatile.
#[test]
fn a_pairing_outranks_the_volatile_name_inventory() {
    let options = Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting(SET_SETTING, "random").holding_set(','),
    );
    let membership = MEMBERSHIP.replace("current_app_subjects()", "random()");
    let postgres = reverse(&format!("SELECT * FROM project_members WHERE {membership}"), &options)
        .expect("the paired name answers the setting, whatever it is called");

    assert_eq!(postgres, format!("SELECT * FROM project_members WHERE {SET_MEMBERSHIP}"));
}

#[test]
fn the_negated_membership_shape_becomes_all_over_the_split() {
    let postgres =
        reverse(&format!("SELECT * FROM project_members WHERE NOT ({MEMBERSHIP})"), &set_paired())
            .expect("the negation is the forward spelling of <> ALL");

    assert_eq!(
        postgres,
        "SELECT * FROM project_members WHERE \
         user_id <> ALL(string_to_array(current_setting('app.subjects', true), ','))"
    );
}

#[test]
fn a_set_valued_setting_read_as_one_value_refuses() {
    let error = reverse(
        "SELECT * FROM project_members WHERE user_id = current_app_subjects()",
        &set_paired(),
    )
    .expect_err("one user_id against the whole joined text matches nobody the server admits");

    assert!(
        matches!(&error, Error::SessionVariableReadAsScalar { pattern, delimiter: ',' }
            if pattern == "current_setting('app.subjects')"),
        "the refusal names the setting and its delimiter, got: {error}"
    );
}

#[test]
fn a_scalar_setting_read_as_a_set_refuses() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_setting(SET_SETTING, SET_FUNCTION));

    let error = reverse(&format!("SELECT * FROM project_members WHERE {MEMBERSHIP}"), &options)
        .expect_err("a mapping that declares no set holds one value");

    assert!(
        matches!(&error, Error::SessionVariableReadAsSet { pattern, written }
            if pattern == "current_setting('app.subjects')" && *written == ','),
        "the refusal names the setting and the delimiter the statement split on, got: {error}"
    );
}

#[test]
fn a_delimiter_other_than_the_declared_one_refuses() {
    let options = Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting(SET_SETTING, SET_FUNCTION).holding_set(';'),
    );

    let error = reverse(&format!("SELECT * FROM project_members WHERE {MEMBERSHIP}"), &options)
        .expect_err("splitting on a comma reads a set the mapping does not declare");

    assert!(
        matches!(&error, Error::SessionVariableDelimiterDisagrees { pattern, recorded: ';', written }
            if pattern == "current_setting('app.subjects')" && *written == ','),
        "the refusal names both delimiters, got: {error}"
    );
}

/// The shape is the forward lowering of `= ANY(string_to_array(a, d))` for any
/// replayable `a`, not only for a setting, so a column reads back the same way.
#[test]
fn the_membership_shape_over_a_column_becomes_any_over_the_split() {
    let postgres = reverse(
        "SELECT * FROM docs WHERE CASE WHEN title IS NOT NULL THEN title <> '' AND \
         instr('x', ';') = 0 AND instr(';' || title || ';', ';' || 'x' || ';') > 0 END",
        &Pg2SqliteOptions::default(),
    )
    .expect("the shape needs no mapping when the text is a column");

    assert_eq!(postgres, "SELECT * FROM docs WHERE 'x' = ANY(string_to_array(title, ';'))");
}

/// The shape reads its text in three places and its left side in two, where
/// PostgreSQL reads each once, so an operand that answers differently per read
/// is not the membership test it looks like.
#[test]
fn the_membership_shape_over_a_volatile_operand_refuses() {
    let error = reverse(
        "SELECT * FROM docs WHERE CASE WHEN title IS NOT NULL THEN title <> '' AND \
         instr(random(), ',') = 0 AND instr(',' || title || ',', ',' || random() || ',') > 0 END",
        &Pg2SqliteOptions::default(),
    )
    .expect_err("random() answers a different value on each read");

    assert!(error.to_string().contains("random()"), "the refusal names the operand: {error}");
}

#[test]
fn the_membership_test_round_trips() {
    let options = set_paired();
    let postgres_source = format!("{DDL} SELECT id FROM project_members WHERE {SET_MEMBERSHIP};");

    let forward = Pg2Sqlite::default()
        .sql(&postgres_source)
        .expect("the document parses")
        .translate(&options)
        .expect("the membership test becomes the guarded search");
    let sqlite_query = forward.last().expect("the query is emitted").to_string();
    assert!(sqlite_query.contains("instr("), "forward emits the search, got: {sqlite_query}");

    let back = reverse(&sqlite_query, &options).expect("and the search becomes the test again");
    assert_eq!(back, format!("SELECT id FROM project_members WHERE {SET_MEMBERSHIP}"));
}
