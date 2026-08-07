//! An unrecognised function name is refused rather than emitted verbatim.
//!
//! The translator used to copy any name it did not recognise straight into the
//! output, so `pg_sleep(1)` reached SQLite and failed there with `no such
//! function`. Under D2 anything unrecognised is a hard error, and the names
//! that keep passing through are the ones SQLite can actually resolve: its own
//! built-ins, and whatever the caller has declared.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};
use rusqlite::Connection;

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, n INT, s TEXT, payload JSONB);";

fn translate(pg: &str, options: &Pg2SqliteOptions) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(pg)
        .map_err(|error| error.to_string())?
        .translate_to_sql(options)
        .map_err(|error| error.to_string())
}

/// Translates `SELECT <call> FROM t` and asserts SQLite resolves every name in
/// the emitted statement, which is what proves the passthrough was justified.
fn assert_sqlite_resolves(call: &str) {
    let options = Pg2SqliteOptions::default();
    let emitted = translate(&format!("{TABLE} SELECT {call} FROM t;"), &options)
        .unwrap_or_else(|error| panic!("{call} should translate: {error}"));

    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &emitted {
        // Emitted SQL is the artifact under test, so it runs as text. `prepare`
        // resolves function names without executing anything.
        connection
            .execute_batch(&format!("{statement};"))
            .or_else(|_| connection.prepare(statement).map(|_| ()))
            .unwrap_or_else(|error| panic!("SQLite rejected {call}: {error}\n{statement}"));
    }
}

#[test]
fn a_postgres_only_function_is_refused() {
    let error =
        translate(&format!("{TABLE} SELECT pg_sleep(1) FROM t;"), &Pg2SqliteOptions::default())
            .expect_err("pg_sleep has no SQLite form and must not be emitted verbatim");
    assert!(error.contains("pg_sleep"), "the error should name the function: {error}");
}

#[test]
fn an_unrecognised_name_is_refused() {
    let error = translate(
        &format!("{TABLE} SELECT definitely_not_a_function(n) FROM t;"),
        &Pg2SqliteOptions::default(),
    )
    .expect_err("an unknown name must not be emitted verbatim");
    assert!(
        error.contains("definitely_not_a_function"),
        "the error should name the function: {error}"
    );
}

/// Guards the refusal from swallowing the functions SQLite does have. Every one
/// of these reaches the same fallback and must keep translating. This passes
/// before the change too.
#[test]
fn sqlite_builtins_still_translate() {
    for call in [
        "abs(n)",
        "coalesce(n, 0)",
        "nullif(n, 0)",
        "ifnull(n, 0)",
        "iif(n > 0, 1, 0)",
        "length(s)",
        "lower(s)",
        "upper(s)",
        "ltrim(s)",
        "rtrim(s)",
        "trim(s)",
        "replace(s, 'a', 'b')",
        "instr(s, 'a')",
        "substr(s, 1, 2)",
        "hex(s)",
        "quote(s)",
        "typeof(n)",
        "unicode(s)",
        "printf('%d', n)",
        "round(n, 1)",
        "random()",
        "last_insert_rowid()",
        "changes()",
        "sqlite_version()",
        "date(s)",
        "time(s)",
        "datetime(s)",
        "julianday(s)",
        "unixepoch(s)",
        "strftime('%Y', s)",
        "json(payload)",
        "json_type(payload)",
        "json_valid(payload)",
        "json_quote(s)",
        "json_array(n)",
        "json_object('a', n)",
    ] {
        assert_sqlite_resolves(call);
    }
}

/// The aggregates and window functions reach the same fallback.
#[test]
fn sqlite_aggregates_and_window_functions_still_translate() {
    for call in [
        "count(*)",
        "sum(n)",
        "avg(n)",
        "min(n)",
        "max(n)",
        "total(n)",
        "group_concat(s)",
        "row_number() OVER ()",
        "rank() OVER (ORDER BY n)",
        "dense_rank() OVER (ORDER BY n)",
        "percent_rank() OVER (ORDER BY n)",
        "cume_dist() OVER (ORDER BY n)",
        "ntile(4) OVER (ORDER BY n)",
        "lag(n) OVER (ORDER BY n)",
        "lead(n) OVER (ORDER BY n)",
        "first_value(n) OVER (ORDER BY n)",
        "last_value(n) OVER (ORDER BY n)",
        "nth_value(n, 2) OVER (ORDER BY n)",
    ] {
        assert_sqlite_resolves(call);
    }
}

/// A host-registered function is the caller's to declare, which is the only
/// way the translator can know SQLite will resolve it.
#[test]
fn a_declared_user_defined_function_translates() {
    let options =
        Pg2SqliteOptions::default().with_user_defined_functions(["my_udf", "another_udf"]);
    let emitted = translate(&format!("{TABLE} SELECT my_udf(n) FROM t;"), &options)
        .expect("a declared function should translate");
    assert!(
        emitted.iter().any(|statement| statement.contains("my_udf")),
        "the declared name should reach the output: {emitted:?}"
    );

    let undeclared = translate(&format!("{TABLE} SELECT not_declared(n) FROM t;"), &options)
        .expect_err("declaring one name must not admit every name");
    assert!(
        undeclared.contains("not_declared"),
        "the error should name the function: {undeclared}"
    );
}

/// The UUID function name is already a declared name, so it must pass through
/// whatever the caller set it to.
#[test]
fn the_configured_uuid_function_name_translates() {
    let options = Pg2SqliteOptions::default().with_uuid_function_name("uuid7");
    let emitted = translate(&format!("{TABLE} SELECT uuid7() FROM t;"), &options)
        .expect("the configured UUID function should translate");
    assert!(
        emitted.iter().any(|statement| statement.contains("uuid7")),
        "the configured name should reach the output: {emitted:?}"
    );
    let conn = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &emitted {
        // uuid7 is a caller-registered UDF absent in the test process; accept
        // that specific error so SQLite still validates the surrounding SQL.
        match conn.execute_batch(&format!("{statement};")) {
            Ok(()) => {}
            Err(e) if e.to_string().contains("no such function: uuid7") => {}
            Err(e) => panic!("SQLite rejected statement: {e}\n{statement}"),
        }
    }
}
