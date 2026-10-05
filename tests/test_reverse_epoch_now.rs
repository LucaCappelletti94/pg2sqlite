//! F25: `unixepoch()` with no arguments, and what `datetime(x)` promises.
//!
//! SQLite's `unixepoch()` answers the current Unix time as whole seconds. The
//! reverse direction demanded exactly one argument, so the commonest spelling
//! of "now, as an integer" had nowhere to go. PostgreSQL's
//! `extract(epoch from now())` answers `1786200937.154282`, so the floor and
//! the cast are both load-bearing.
//!
//! The one-argument `datetime(x)` is the other half of the item and keeps its
//! behaviour. It is exact for a zone-aware operand and, for a zone-free one,
//! right about the clock reading while turning a plain timestamp into a
//! zone-aware one. That is documented rather than coded around, because the
//! forward direction emits this same call for both, so no reversal can be
//! right about both.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;
use sql_traits::structs::ParserDB;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, ts TIMESTAMP, tz TIMESTAMPTZ);";

fn schema() -> ParserDB {
    Pg2Sqlite::default().sql(SCHEMA).expect("parse").build_schema().expect("build")
}

fn reverse(sqlite: &str) -> String {
    let statements = Pg2Sqlite::default()
        .reverse_sql(sqlite, &schema(), &Pg2SqliteOptions::default())
        .expect("reverse translation");
    let sql = statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    Parser::parse_sql(&PostgreSqlDialect {}, &sql).expect("output parses as PostgreSQL");
    sql
}

// --- unixepoch() with no arguments -----------------------------------------

#[test]
fn a_bare_unixepoch_becomes_the_current_epoch_in_whole_seconds() {
    let pg = reverse("SELECT unixepoch() FROM t");
    assert!(pg.contains("floor"), "the fraction has to go: {pg}");
    assert!(pg.contains("EPOCH"), "{pg}");
    assert!(pg.contains("NOW()"), "{pg}");
    assert!(pg.contains("BIGINT"), "SQLite answers an integer: {pg}");
}

/// The whole point of the floor: SQLite's answer has no fraction.
#[test]
fn sqlite_answers_whole_seconds() {
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    let kind: String =
        connection.query_row("SELECT typeof(unixepoch())", [], |row| row.get(0)).expect("typeof");
    assert_eq!(kind, "integer");
}

#[test]
fn the_one_argument_form_still_becomes_extract() {
    let pg = reverse("SELECT unixepoch(ts) FROM t");
    assert!(pg.contains("EXTRACT(EPOCH FROM ts)"), "{pg}");
}

#[test]
fn the_subsec_form_still_becomes_extract() {
    let pg = reverse("SELECT unixepoch(ts, 'subsec') FROM t");
    assert!(pg.contains("EXTRACT(EPOCH FROM ts)"), "{pg}");
}

/// A modifier that is not `subsec` still has nowhere to go, and the message no
/// longer claims the zero-argument form is one of the refused shapes.
#[test]
fn an_unknown_modifier_is_still_refused() {
    let error = Pg2Sqlite::default()
        .reverse_sql("SELECT unixepoch(ts, 'utc') FROM t", &schema(), &Pg2SqliteOptions::default())
        .expect_err("only subsec reverses")
        .to_string();
    assert!(error.contains("subsec"), "{error}");
}

// --- datetime(x), unchanged and documented ---------------------------------

#[test]
fn a_one_argument_datetime_still_becomes_at_time_zone_utc() {
    let pg = reverse("SELECT datetime(tz) FROM t");
    assert!(pg.contains("AT TIME ZONE 'UTC'"), "{pg}");
}

/// The documented divergence, pinned so the documentation cannot drift away
/// from it: a zone-free operand takes the same reversal.
#[test]
fn a_zone_free_operand_takes_the_same_reversal() {
    let pg = reverse("SELECT datetime(ts) FROM t");
    assert!(pg.contains("AT TIME ZONE 'UTC'"), "{pg}");
}
