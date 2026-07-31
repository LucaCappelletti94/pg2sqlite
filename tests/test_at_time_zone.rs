//! `AT TIME ZONE`, which is two operations sharing one syntax.
//!
//! Measured on PostgreSQL 16 with the session zone at UTC, over the wall clock
//! `2023-01-15 12:00:00`:
//!
//! | expression | answer | shift |
//! |---|---|---|
//! | `TIMESTAMP ... AT TIME ZONE '+05:30'` | 17:30 | plus |
//! | `TIMESTAMPTZ ... AT TIME ZONE '+05:30'` | 06:30 | minus |
//! | `TIMESTAMP ... AT TIME ZONE 'UTC'` | 12:00 | none |
//! | `TIMESTAMPTZ ... AT TIME ZONE 'UTC'` | 12:00 | none |
//! | `TIMESTAMP ... AT TIME ZONE 'utc+02:30'` | 14:30 | plus |
//!
//! The plus on the first row is not a typo. PostgreSQL reads a bare `'+05:30'`
//! STRING as a POSIX zone specification, where the sign is the opposite of the
//! ISO one, so that zone is UTC-5:30 and reading 12:00 as local to it gives
//! 17:30 UTC. `AT TIME ZONE INTERVAL '05:30'` and `AT TIME ZONE 'Asia/Kolkata'`
//! both answer 06:30 instead, and neither spelling reaches SQLite.
//!
//! SQLite's `'utc'` modifier is not a no-op either: it reads the value as local
//! time and converts, so it shifts by whatever offset the machine running the
//! query happens to have.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, naive TIMESTAMP, aware TIMESTAMPTZ);
     INSERT INTO t VALUES (1, '2023-01-15 12:00:00', '2023-01-15 12:00:00');";

fn evaluate(expression: &str) -> Option<String> {
    run_translated_with(
        &format!("{TABLE} SELECT {expression} FROM t;"),
        &Pg2SqliteOptions::default(),
    )
    .into_iter()
    .next()
    .expect("one row")
}

fn refuse(expression: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{TABLE} SELECT {expression} FROM t;"))
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("this operand has no determinable direction")
        .to_string()
}

/// A naive timestamp is read as local to the zone, so the offset is added.
#[test]
fn a_naive_timestamp_moves_toward_utc() {
    assert_eq!(evaluate("naive AT TIME ZONE '+05:30'"), Some("2023-01-15 17:30:00".to_string()));
    assert_eq!(evaluate("naive AT TIME ZONE '-05:30'"), Some("2023-01-15 06:30:00".to_string()));
}

/// An aware timestamp is already UTC, so converting it into the zone subtracts.
/// This is the direction the translator had backwards.
#[test]
fn an_aware_timestamp_moves_away_from_utc() {
    assert_eq!(evaluate("aware AT TIME ZONE '+05:30'"), Some("2023-01-15 06:30:00".to_string()));
    assert_eq!(evaluate("aware AT TIME ZONE '-05:30'"), Some("2023-01-15 17:30:00".to_string()));
}

/// UTC shifts nothing in PostgreSQL, in either direction. SQLite's `'utc'`
/// modifier shifts by the machine's own offset, so emitting it made the answer
/// depend on where the query ran.
#[test]
fn utc_shifts_nothing() {
    for expression in [
        "naive AT TIME ZONE 'UTC'",
        "aware AT TIME ZONE 'UTC'",
        "naive AT TIME ZONE 'GMT'",
        "naive AT TIME ZONE '+00:00'",
    ] {
        assert_eq!(evaluate(expression), Some("2023-01-15 12:00:00".to_string()), "{expression}");
    }
}

/// The `utc±HH:MM` spelling carries the same sign as the bare one.
#[test]
fn a_utc_prefixed_offset_reads_the_same_way() {
    assert_eq!(evaluate("naive AT TIME ZONE 'utc+02:30'"), Some("2023-01-15 14:30:00".to_string()));
}

/// Guessing the direction is wrong half the time, so an operand whose type
/// cannot be resolved is refused rather than shifted one way and hoped for.
#[test]
fn an_operand_of_unknown_type_is_refused() {
    let error = refuse("(SELECT max(naive) FROM t) AT TIME ZONE '+05:30'");
    assert!(error.contains("AT TIME ZONE"), "the error must name the construct, got: {error}");
}

/// A named zone still has no SQLite equivalent.
#[test]
fn a_named_zone_is_refused() {
    let error = refuse("naive AT TIME ZONE 'America/New_York'");
    assert!(error.contains("AT TIME ZONE"), "got: {error}");
}
