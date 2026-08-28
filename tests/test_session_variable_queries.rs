//! The caller's identity inside a plain query, rather than inside a policy.
//!
//! A session variable mapping pairs a PostgreSQL setting with the function the
//! replica registers. Until now that pairing was read only while translating a
//! row-security policy, so a query naming the caller was refused as an unknown
//! function. These tests cover the query path: the pairing applies, an
//! unmapped pattern refuses in the mapping's own words, and a cast that
//! disagrees with the type the pairing records refuses rather than being
//! dropped in silence.
//!
//! Each behavioural case executes the translator's own emitted SQL against a
//! SQLite connection with the paired function registered, and discriminates by
//! switching the session between two rows.

mod helpers;

use diesel::{prelude::*, sql_query};
use helpers::{establish_connection, set_session_username};
use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping},
};

diesel::table! {
    /// The rows the emitted query reads, seeded through the typed DSL.
    docs (id) {
        /// Row identifier.
        id -> Integer,
        /// Who the row belongs to, compared against the caller.
        owner -> Text,
    }
}

/// One table and two rows owned by different callers, so a query that filters
/// on the caller answers differently for each.
const PG_SCHEMA: &str = "CREATE TABLE docs (id INT PRIMARY KEY, owner TEXT NOT NULL);";

/// The function the test connection registers, driven by
/// [`set_session_username`].
const PAIRED_FUNCTION: &str = "current_app_username";

/// The setting the pairing names.
const SETTING: &str = "app.username";

/// A single-column read of the emitted query's answer.
#[derive(QueryableByName, Debug)]
struct OwnerRow {
    /// The identifier the emitted projection returns.
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
}

fn setting_paired() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_setting(SETTING, PAIRED_FUNCTION))
}

fn translate(pg: &str, options: &Pg2SqliteOptions) -> Result<Vec<String>, Error> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("the document parses as PostgreSQL")
        .translate(options)
        .map(|statements| statements.iter().map(ToString::to_string).collect())
}

/// Applies the emitted schema, seeds one row per owner, then runs the emitted
/// query for `caller` and returns the identifiers it answers.
///
/// The emitted statements are generated text, which the typed DSL cannot
/// express by construction, so they go through `sql_query`. Everything the test
/// itself states, the seed rows and the read of the answer, stays typed.
fn ids_visible_to(emitted: &[String], caller: &str) -> Vec<i32> {
    let (query, setup) = emitted.split_last().expect("the document emits at least one statement");

    let mut connection = establish_connection();
    for statement in setup {
        sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted setup failed: {error}\n{statement}"));
    }

    diesel::insert_into(docs::table)
        .values(vec![
            (docs::id.eq(1), docs::owner.eq("alice")),
            (docs::id.eq(2), docs::owner.eq("bob")),
        ])
        .execute(&mut connection)
        .expect("seed both owners");

    set_session_username(caller);
    sql_query(query)
        .load::<OwnerRow>(&mut connection)
        .unwrap_or_else(|error| panic!("emitted query failed: {error}\n{query}"))
        .into_iter()
        .map(|row| row.id)
        .collect()
}

#[test]
fn a_query_naming_the_setting_calls_the_paired_function() {
    let emitted = translate(
        &format!("{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_setting('{SETTING}');"),
        &setting_paired(),
    )
    .expect("the pairing says what the setting becomes");

    let query = emitted.last().expect("the query is emitted");
    assert!(
        query.contains(&format!("{PAIRED_FUNCTION}()")) && !query.contains("current_setting"),
        "the setting becomes the paired call, got: {query}"
    );
    assert_eq!(ids_visible_to(&emitted, "alice"), vec![1]);
    assert_eq!(ids_visible_to(&emitted, "bob"), vec![2]);
}

#[test]
fn a_query_naming_current_user_calls_the_paired_function() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_user(PAIRED_FUNCTION));
    let emitted = translate(
        &format!("{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_user;"),
        &options,
    )
    .expect("the pairing says what current_user becomes");

    let query = emitted.last().expect("the query is emitted");
    assert!(
        query.contains(&format!("{PAIRED_FUNCTION}()")),
        "current_user becomes the paired call, got: {query}"
    );
    assert_eq!(ids_visible_to(&emitted, "alice"), vec![1]);
    assert_eq!(ids_visible_to(&emitted, "bob"), vec![2]);
}

#[test]
fn a_cast_over_the_setting_drops_the_cast_when_the_pairing_agrees() {
    let options = Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting(SETTING, PAIRED_FUNCTION).with_pg_type("text"),
    );
    let emitted = translate(
        &format!(
            "{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_setting('{SETTING}')::text;"
        ),
        &options,
    )
    .expect("the written cast agrees with the recorded type");

    let query = emitted.last().expect("the query is emitted");
    assert!(
        !query.contains("CAST"),
        "the replica's function needs no cast, so it is dropped, got: {query}"
    );
    assert_eq!(ids_visible_to(&emitted, "alice"), vec![1]);
}

#[test]
fn a_cast_over_the_setting_needs_no_recorded_type() {
    let emitted = translate(
        &format!(
            "{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_setting('{SETTING}')::text;"
        ),
        &setting_paired(),
    )
    .expect("a pairing that records no type has nothing to disagree with");

    assert_eq!(ids_visible_to(&emitted, "bob"), vec![2]);
}

#[test]
fn one_type_spelled_two_ways_agrees() {
    let options = Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting(SETTING, PAIRED_FUNCTION).with_pg_type("integer"),
    );

    translate(
        &format!("{PG_SCHEMA} SELECT id FROM docs WHERE id = current_setting('{SETTING}')::int;"),
        &options,
    )
    .expect("`int` and `integer` are one PostgreSQL type spelled two ways");
}

#[test]
fn a_cast_that_disagrees_with_the_pairing_refuses() {
    let options = Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting(SETTING, PAIRED_FUNCTION).with_pg_type("uuid"),
    );

    let error = translate(
        &format!(
            "{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_setting('{SETTING}')::text;"
        ),
        &options,
    )
    .expect_err("the recorded type and the written cast disagree");

    assert!(
        matches!(&error, Error::SessionVariableTypeDisagrees { pattern, recorded, written }
            if pattern.contains(SETTING) && recorded == "uuid" && written == "TEXT"),
        "the refusal names both types, got: {error}"
    );
}

#[test]
fn a_query_naming_an_unpaired_setting_refuses() {
    let error = translate(
        &format!("{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_setting('{SETTING}');"),
        &Pg2SqliteOptions::default(),
    )
    .expect_err("nothing says what the setting becomes");

    assert!(
        matches!(&error, Error::SessionVariableMappingNotFound { pattern }
            if pattern.contains(SETTING)),
        "the refusal is the mapping's own, not a generic unknown function, got: {error}"
    );
}

#[test]
fn a_query_naming_an_unpaired_current_user_refuses() {
    let error = translate(
        &format!("{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_user;"),
        &Pg2SqliteOptions::default(),
    )
    .expect_err("nothing says what current_user becomes");

    assert!(
        matches!(&error, Error::SessionVariableMappingNotFound { pattern }
            if pattern.contains("current_user")),
        "the refusal is the mapping's own, got: {error}"
    );
}

#[test]
fn a_declared_current_setting_passes_through_unpaired() {
    let options = Pg2SqliteOptions::default().with_user_defined_functions(["current_setting"]);
    let emitted = translate(
        &format!("{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_setting('{SETTING}');"),
        &options,
    )
    .expect("a declared name is evidence the destination has it");

    let query = emitted.last().expect("the query is emitted");
    assert!(
        query.contains("current_setting"),
        "a caller who registered the name keeps it, got: {query}"
    );
}

#[test]
fn a_setting_named_by_an_expression_refuses() {
    let error = translate(
        &format!("{PG_SCHEMA} SELECT id FROM docs WHERE owner = current_setting(owner);"),
        &setting_paired(),
    )
    .expect_err("a setting name computed at run time can never be paired");

    assert!(
        format!("{error}").contains("current_setting"),
        "the refusal names the function, got: {error}"
    );
}

/// M8: a strict mapping reverses to `current_setting(name)` with no second
/// argument. The tolerant form (the default) adds `true` as the second arg.
#[test]
fn a_strict_mapping_reverses_to_one_argument_current_setting() {
    const STRICT_SETTING: &str = "app.strict_user";
    const STRICT_FN: &str = "strict_user_fn";

    let translator = Pg2Sqlite::default().sql(PG_SCHEMA).expect("parse");
    let schema = translator.build_schema().expect("schema");
    let options = Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting_strict(STRICT_SETTING, STRICT_FN),
    );

    let sqlite_sql = format!("SELECT {STRICT_FN}() FROM docs;");
    let reversed = translator
        .reverse_sql(&sqlite_sql, &schema, &options)
        .expect("reverse translate")
        .into_iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("; ");

    assert!(
        reversed.contains(&format!("current_setting('{STRICT_SETTING}')")),
        "strict mapping must restore one-argument current_setting: {reversed}"
    );
    assert!(
        !reversed.contains("true"),
        "strict mapping must not include missing_ok argument: {reversed}"
    );
}
