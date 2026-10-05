//! Neither direction may know a name the other does not.
//!
//! `tests/gauntlet/reverse.rs` closes this going back to PostgreSQL, using the
//! server's catalogue as its corpus. Going out to SQLite there is no server to
//! ask, because the corpus is not "what SQLite has" in the abstract but what
//! this crate claims it has, so the corpus is the crate's own inventory and the
//! two sweeps below read it rather than keep a copy.
//!
//! Both are behavioural: they run the translators and look at what comes back,
//! so neither can pass by agreeing with a list.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;

const DDL: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT, n INT, r REAL, bin BYTEA, \
                   ts TIMESTAMP, payload JSONB);";

/// Every capability declared, since a refusal for want of an opt-in says
/// nothing about whether the two directions agree on a name.
fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_math_functions_available()
}

fn schema() -> ParserDB {
    Pg2Sqlite::default()
        .sql(DDL)
        .expect("fixture parses")
        .build_schema()
        .expect("fixture builds a schema")
}

/// The PostgreSQL a SQLite call reverses into, or the refusal.
fn reverse(sqlite: &str) -> Result<String, pg2sqlite::errors::Error> {
    let statements = Pg2Sqlite::default().reverse_sql(sqlite, &schema(), &options())?;
    Ok(statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
}

fn forward(postgres: &str) -> Result<String, pg2sqlite::errors::Error> {
    let statements =
        Pg2Sqlite::default().sql(&format!("{DDL}{postgres};"))?.translate_to_sql(&options())?;
    Ok(statements.last().cloned().unwrap_or_default())
}

// H6: the crate's own forward emission for `json_agg` must survive the round
// trip. Forward emits `NULLIF(json_group_array(x), '[]')`. Reverse renames the
// inner call back to `json_agg` but leaves the NULLIF wrapper intact, producing
// `NULLIF(json_agg(x), '[]')`. PostgreSQL refuses that expression because the
// `json` type has no equality operator, so NULLIF cannot compare.
//
// Verified failing on postgres:18: `operator does not exist: json = unknown`.

/// The crate's own forward emission for `json_agg` must reverse into plain
/// `json_agg(x)` with no NULLIF wrapper. Currently reverse emits
/// `NULLIF(json_agg(x), '[]')`, which PostgreSQL refuses.
#[test]
fn json_agg_forward_emission_round_trips_without_nullif() {
    let sqlite_sql = forward("SELECT json_agg(s) FROM t").expect("forward json_agg must succeed");
    let pg_sql =
        reverse(&sqlite_sql).expect("the crate's own json_agg emission must reverse successfully");
    assert!(pg_sql.contains("json_agg"), "reverse output must call json_agg, got: {pg_sql}");
    assert!(
        !pg_sql.contains("NULLIF"),
        "reverse output must not wrap json_agg in NULLIF (PostgreSQL json has no equality \
         operator), got: {pg_sql}"
    );
}

// M6: forward emissions for `localtimestamp` and `localtime` must survive the
// round trip. Forward maps `localtimestamp` to `datetime('now', 'localtime')`
// and `localtime` to `time('now', 'localtime')`. The reverse direction
// currently refuses both forms, breaking direction parity.

/// `localtimestamp` forward-emits `datetime('now', 'localtime')`. The reverse
/// direction must recognise that idiom and emit `LOCALTIMESTAMP` again.
/// Currently the reverse direction refuses.
#[test]
fn localtimestamp_forward_emission_round_trips() {
    let sqlite_sql = forward("SELECT localtimestamp").expect("forward localtimestamp must succeed");
    let result = reverse(&sqlite_sql);
    assert!(
        result.is_ok(),
        "the crate's own localtimestamp emission must reverse successfully, got: {:?}",
        result
    );
    let pg_sql = result.unwrap();
    assert!(
        pg_sql.to_uppercase().contains("LOCALTIMESTAMP"),
        "reverse output must contain LOCALTIMESTAMP, got: {pg_sql}"
    );
}

/// `localtime` forward-emits `time('now', 'localtime')`. The reverse direction
/// must recognise that idiom and emit `LOCALTIME` again. Currently the reverse
/// direction refuses.
#[test]
fn localtime_forward_emission_round_trips() {
    let sqlite_sql = forward("SELECT localtime").expect("forward localtime must succeed");
    let result = reverse(&sqlite_sql);
    assert!(
        result.is_ok(),
        "the crate's own localtime emission must reverse successfully, got: {:?}",
        result
    );
    let pg_sql = result.unwrap();
    assert!(
        pg_sql.to_uppercase().contains("LOCALTIME"),
        "reverse output must contain LOCALTIME, got: {pg_sql}"
    );
}

// H2-reverse: the crate's own forward emission for `json_object_agg` must
// survive the round trip. After FixForward lands, the forward direction will
// emit `NULLIF(json_group_object(k, v), '{}')`. The NULLIF wrapper breaks
// PostgreSQL (json has no equality operator), so the reverse must strip it and
// restore plain `json_object_agg(k, v)`.
//
// The contract test below exercises the end-to-end round trip. Because
// FixForward runs concurrently, the forward emission may still be bare
// `json_group_object(k, v)` when this test runs; in that state the round trip
// passes for a different (correct) reason. A second test exercises the reverse
// recognizer directly against the contracted NULLIF shape.

/// Forward-translate `json_object_agg(k, v)` and reverse the emission.
/// The result must restore `json_object_agg` with no `NULLIF` wrapper.
#[test]
fn json_object_agg_forward_emission_round_trips_without_nullif() {
    let sqlite_sql = forward("SELECT json_object_agg(k, v) FROM t")
        .expect("forward json_object_agg must succeed");
    let pg_sql = reverse(&sqlite_sql)
        .expect("the crate's own json_object_agg emission must reverse successfully");
    assert!(
        pg_sql.contains("json_object_agg"),
        "reverse output must call json_object_agg, got: {pg_sql}"
    );
    assert!(
        !pg_sql.contains("NULLIF"),
        "reverse output must not wrap json_object_agg in NULLIF, got: {pg_sql}"
    );
}

/// Exercises the NULLIF-recognizer directly against the contracted shape that
/// FixForward will emit, independent of whether that change has landed yet.
/// This is the unit proof that the reverse recognizer handles
/// `NULLIF(json_group_object(k, v), '{}')` correctly.
#[test]
fn nullif_json_group_object_reverses_to_json_object_agg() {
    let pg_sql = reverse("SELECT NULLIF(json_group_object(k, v), '{}') FROM t")
        .expect("NULLIF(json_group_object(k, v), '{}') must reverse successfully");
    assert!(
        pg_sql.contains("json_object_agg"),
        "reverse output must call json_object_agg, got: {pg_sql}"
    );
    assert!(
        !pg_sql.contains("NULLIF"),
        "reverse output must not contain NULLIF wrapper, got: {pg_sql}"
    );
}
