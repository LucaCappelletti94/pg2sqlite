//! Guards on `make_date`, `make_time`, `make_timestamp`, `greatest`, `least`,
//! `left`, `right`, and `cbrt`: each lowering names an operand in more than
//! one output position, so a volatile operand would be evaluated more often
//! than PostgreSQL evaluates it, and the copies would answer from draws
//! PostgreSQL never made.

use diesel::{RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate_err(pg: &str, options: &Pg2SqliteOptions) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(options)
        .expect_err("expected refusal")
        .to_string()
}

fn translate_ok(pg: &str, options: &Pg2SqliteOptions) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(options)
        .expect("expected translation to succeed")
        .join("\n")
}

/// A single text-typed column named `result`, used to read the translated
/// scalar expression from SQLite.
///
/// `QueryableByName` rather than the typed table DSL because the executed
/// statement is dynamically generated translator output, unknown at compile
/// time, so the typed DSL cannot express it.
#[derive(QueryableByName, Debug)]
struct TextResult {
    #[diesel(sql_type = diesel::sql_types::Text)]
    result: String,
}

/// Translates `pg`, executes the emitted SQL through an in-memory SQLite
/// connection, and returns the first column of the first row as a string.
///
/// The statement is the translator's own output, so `diesel::sql_query` is the
/// correct form: the typed DSL cannot express a dynamically generated string.
fn exec_scalar(sql: &str) -> String {
    let mut conn = SqliteConnection::establish(":memory:").expect("in-memory SQLite");
    diesel::sql_query(sql)
        .load::<TextResult>(&mut conn)
        .unwrap_or_else(|e| panic!("query failed: {e}\n{sql}"))
        .into_iter()
        .next()
        .expect("expected at least one row")
        .result
}

// ── C1: make_time
// ─────────────────────────────────────────────────────────────

/// `make_time(h, m, s)` names each argument in both the NULL-presence guard and
/// the format expression. A volatile argument for `s` would be evaluated four
/// times at runtime where PostgreSQL evaluates it once, answering a time whose
/// seconds component advances with each draw.
#[test]
fn make_time_refuses_volatile_seconds() {
    let err = translate_err("SELECT make_time(12, 0, random())", &Pg2SqliteOptions::default());
    assert!(err.contains("make_time"), "error should name make_time: {err}");
}

/// Literal arguments are replayable; the ordinary `make_time` still translates
/// and the emitted SQL executes to the expected time string.
#[test]
fn make_time_with_literal_args_executes() {
    let sql = translate_ok("SELECT make_time(12, 30, 15) AS result", &Pg2SqliteOptions::default());
    assert_eq!(exec_scalar(&sql), "12:30:15");
}

// ── C1: make_date
// ─────────────────────────────────────────────────────────────

/// `make_date(y, m, d)` names each argument in the NULL guard and the format
/// string. A volatile year would cause the date to be constructed from draws
/// that disagree.
#[test]
fn make_date_refuses_volatile_year() {
    let err = translate_err("SELECT make_date(random()::int, 1, 1)", &Pg2SqliteOptions::default());
    assert!(err.contains("make_date"), "error should name make_date: {err}");
}

/// Literal arguments are replayable; the ordinary `make_date` executes and
/// returns the expected ISO date string.
#[test]
fn make_date_with_literal_args_executes() {
    let sql = translate_ok("SELECT make_date(2024, 3, 15) AS result", &Pg2SqliteOptions::default());
    assert_eq!(exec_scalar(&sql), "2024-03-15");
}

// ── C2: make_timestamp
// ────────────────────────────────────────────────────────

/// `make_timestamp` names each of its six arguments in both the NULL guard and
/// the format string. A volatile seconds argument would be evaluated four
/// times, assembling a timestamp whose seconds field disagrees across its four
/// reads.
#[test]
fn make_timestamp_refuses_volatile_seconds() {
    let err = translate_err(
        "SELECT make_timestamp(2024, 1, 1, 12, 0, random())",
        &Pg2SqliteOptions::default(),
    );
    assert!(err.contains("make_timestamp"), "error should name make_timestamp: {err}");
}

/// Literal arguments are replayable; the ordinary `make_timestamp` executes
/// and returns the expected timestamp string.
#[test]
fn make_timestamp_with_literal_args_executes() {
    let sql = translate_ok(
        "SELECT make_timestamp(2024, 3, 15, 12, 30, 45) AS result",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(exec_scalar(&sql), "2024-03-15 12:30:45");
}

// ── C3: greatest / least
// ──────────────────────────────────────────────────────

/// `greatest` with two or more arguments emits N `coalesce` rotations; each
/// argument appears in N of them. A volatile argument would be read N times,
/// one per rotation that reaches it before finding a non-NULL value.
#[test]
fn greatest_refuses_volatile_argument() {
    let err = translate_err("SELECT greatest(random(), 0.5)", &Pg2SqliteOptions::default());
    assert!(err.contains("greatest"), "error should name greatest: {err}");
}

/// Literal arguments are replayable; `greatest` still translates and returns
/// the larger value.
#[test]
fn greatest_with_literal_args_executes() {
    // CAST to TEXT so the column type matches TextResult.
    let sql =
        translate_ok("SELECT CAST(greatest(5, 3) AS TEXT) AS result", &Pg2SqliteOptions::default());
    assert_eq!(exec_scalar(&sql), "5");
}

/// A single-argument `greatest` returns its argument directly; there is no
/// rotation and no copy, so a volatile single argument is not refused.
#[test]
fn greatest_single_volatile_argument_passes() {
    translate_ok("SELECT greatest(random())", &Pg2SqliteOptions::default());
}

/// `least` applies the same rotation strategy; a volatile argument is refused.
#[test]
fn least_refuses_volatile_argument() {
    let err = translate_err("SELECT least(random(), 0.5)", &Pg2SqliteOptions::default());
    assert!(err.contains("least"), "error should name least: {err}");
}

/// Literal arguments are replayable; `least` still translates and returns the
/// smaller value.
#[test]
fn least_with_literal_args_executes() {
    let sql =
        translate_ok("SELECT CAST(least(5, 3) AS TEXT) AS result", &Pg2SqliteOptions::default());
    assert_eq!(exec_scalar(&sql), "3");
}

// ── C4: left / right ─────────────────────────────────────────────────────────

/// `left(s, n)` names `n` in the sign test (`n < 0`) and again in the length
/// expression (both the positive and negative branches). A volatile `n` is
/// evaluated twice, and the two draws may disagree, so the returned string may
/// not match what PostgreSQL would return.
#[test]
fn left_refuses_volatile_n() {
    let err = translate_err("SELECT left('hello', random()::int)", &Pg2SqliteOptions::default());
    assert!(err.contains("left"), "error should name left: {err}");
}

/// Literal arguments are replayable; `left` still translates and returns the
/// expected prefix.
#[test]
fn left_with_literal_args_executes() {
    let sql = translate_ok("SELECT left('hello', 2) AS result", &Pg2SqliteOptions::default());
    assert_eq!(exec_scalar(&sql), "he");
}

/// `right(s, n)` names `n` in the sign test and in the length branches as
/// well. A volatile `n` evaluated twice may produce inconsistent results.
#[test]
fn right_refuses_volatile_n() {
    let err = translate_err("SELECT right('hello', random()::int)", &Pg2SqliteOptions::default());
    assert!(err.contains("right"), "error should name right: {err}");
}

/// Literal arguments are replayable; `right` still translates and returns the
/// expected suffix.
#[test]
fn right_with_literal_args_executes() {
    let sql = translate_ok("SELECT right('hello', 2) AS result", &Pg2SqliteOptions::default());
    assert_eq!(exec_scalar(&sql), "lo");
}

// ── C5: cbrt ─────────────────────────────────────────────────────────────────

/// `cbrt(x)` emits `sign(x) * pow(abs(x), 1.0/3.0)`: `x` appears at two AST
/// positions (once for the sign and once inside `abs`). A volatile operand
/// would be evaluated twice, reading the sign of one draw and the magnitude of
/// another.
#[test]
fn cbrt_refuses_volatile_operand() {
    let err = translate_err(
        "SELECT cbrt(random())",
        &Pg2SqliteOptions::default().with_math_functions_available(),
    );
    assert!(err.contains("cbrt"), "error should name cbrt: {err}");
}

/// A literal operand is replayable; `cbrt` still translates to the sign-aware
/// form. The default `rusqlite` bundled build omits `pow`, so the emitted SQL
/// is asserted by text rather than executed.
#[test]
fn cbrt_with_literal_arg_emits_signed_form() {
    let sql = translate_ok(
        "SELECT cbrt(8)",
        &Pg2SqliteOptions::default().with_math_functions_available(),
    );
    assert!(
        sql.contains("sign(8)") && sql.contains("pow(abs(8)"),
        "expected sign and pow in emitted form: {sql}"
    );
}
