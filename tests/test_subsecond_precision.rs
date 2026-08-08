//! Fractional seconds survive `extract(epoch)`, `to_timestamp`, `make_time`
//! and `make_timestamp`.
//!
//! All four used to round to whole seconds without saying so:
//! `strftime('%s', ...)` has no fractional part, `datetime(e, 'unixepoch')`
//! renders none, and `printf('%02d', ...)` truncates the argument it is given.
//!
//! SQLite stops at milliseconds where PostgreSQL carries microseconds, so the
//! two agree to three decimal places and no further. That ceiling is pinned
//! here and documented in the README, since only the timestamp paths hit it:
//! `make_time` formats its own argument and never goes through SQLite's date
//! machinery, so it keeps a microsecond.
//!
//! Every expected value below was read off PostgreSQL 17 before the change.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::{Connection, types::FromSql};

const TABLE: &str = "CREATE TABLE t (ts TIMESTAMP, n DOUBLE PRECISION);";

fn translate(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{TABLE}\n{pg}"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .pop()
        .expect("a probe statement")
}

fn evaluate<T: FromSql>(pg: &str) -> T {
    let probe = translate(pg);
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    connection
        .query_row(&probe, [], |row| row.get::<_, T>(0))
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"))
}

/// Reverse translates one SQLite statement back to PostgreSQL.
fn reverse(sqlite: &str) -> String {
    let translator = Pg2Sqlite::default().sql(TABLE).expect("parse the schema");
    let schema = translator.build_schema().expect("build the schema");
    translator
        .reverse_sql(sqlite, &schema, &Pg2SqliteOptions::default())
        .expect("reverse translate")
        .pop()
        .expect("a statement")
        .to_string()
}

// ---------- extract(epoch) ----------

/// PostgreSQL answers 1709647629.500000.
#[test]
fn extract_epoch_keeps_the_fraction() {
    let answer: f64 = evaluate("SELECT extract(epoch from timestamp '2024-03-05 14:07:09.5');");
    assert!((answer - 1_709_647_629.5).abs() < 1e-6, "got {answer}");
}

#[test]
fn date_part_epoch_keeps_the_fraction() {
    let answer: f64 = evaluate("SELECT date_part('epoch', timestamp '2024-03-05 14:07:09.5');");
    assert!((answer - 1_709_647_629.5).abs() < 1e-6, "got {answer}");
}

/// PostgreSQL answers 1709647629 for a whole second and 1709596800 for a date.
#[test]
fn epoch_without_a_fraction_is_unchanged() {
    let whole: f64 = evaluate("SELECT extract(epoch from timestamp '2024-03-05 14:07:09');");
    assert!((whole - 1_709_647_629.0).abs() < 1e-6, "got {whole}");
    let day: f64 = evaluate("SELECT extract(epoch from date '2024-03-05');");
    assert!((day - 1_709_596_800.0).abs() < 1e-6, "got {day}");
}

/// F1 intercepts a difference of two timestamps as a pair, before either call
/// is reversed on its own. That interception must survive this item, which
/// makes the crate emit the same spelling on its own for the first time.
#[test]
fn the_paired_epoch_difference_is_still_intercepted_whole() {
    assert_eq!(
        translate(
            "SELECT extract(epoch from (timestamp '2024-03-05 14:07:09.5' \
             - timestamp '2024-03-05 14:07:09'));"
        ),
        "SELECT (unixepoch(CAST('2024-03-05 14:07:09.5' AS TEXT), 'subsec') \
         - unixepoch(CAST('2024-03-05 14:07:09' AS TEXT), 'subsec'))"
    );
}

// ---------- to_timestamp ----------

/// PostgreSQL answers 2024-03-05 14:07:09.5+00, and a translated timestamp
/// literal keeps its own `.5`, so the two texts have to agree to compare equal.
#[test]
fn to_timestamp_keeps_the_fraction() {
    assert_eq!(evaluate::<String>("SELECT to_timestamp(1709647629.5);"), "2024-03-05 14:07:09.5");
}

/// PostgreSQL answers 2024-03-05 14:07:09+00, with no fractional part at all.
#[test]
fn to_timestamp_of_a_whole_second_renders_no_fraction() {
    assert_eq!(evaluate::<String>("SELECT to_timestamp(1709647629);"), "2024-03-05 14:07:09");
    assert_eq!(evaluate::<String>("SELECT to_timestamp(1709647630);"), "2024-03-05 14:07:10");
    assert_eq!(evaluate::<String>("SELECT to_timestamp(0);"), "1970-01-01 00:00:00");
}

// ---------- make_time and make_timestamp ----------

/// PostgreSQL answers 08:15:00.5.
#[test]
fn make_time_keeps_the_fraction() {
    assert_eq!(evaluate::<String>("SELECT make_time(8, 15, 0.5);"), "08:15:00.5");
}

/// PostgreSQL answers 08:15:07, with no fractional part, so a fixed decimal
/// format would be a new divergence rather than a fix.
#[test]
fn make_time_of_whole_seconds_renders_no_fraction() {
    assert_eq!(evaluate::<String>("SELECT make_time(8, 15, 7);"), "08:15:07");
    assert_eq!(evaluate::<String>("SELECT make_time(8, 15, 0);"), "08:15:00");
}

/// PostgreSQL answers 08:15:00.000001 and 08:15:59.999999. `make_time` formats
/// its own argument, so it is exact to the microsecond where the timestamp
/// paths stop at the millisecond.
#[test]
fn make_time_keeps_a_microsecond() {
    assert_eq!(evaluate::<String>("SELECT make_time(8, 15, 0.000001);"), "08:15:00.000001");
    assert_eq!(evaluate::<String>("SELECT make_time(8, 15, 59.999999);"), "08:15:59.999999");
}

/// PostgreSQL answers 2024-03-05 14:07:09.5 and 2024-03-05 14:07:09. The item
/// named only `make_time`, and the same builder serves this one.
#[test]
fn make_timestamp_keeps_the_fraction() {
    assert_eq!(
        evaluate::<String>("SELECT make_timestamp(2024, 3, 5, 14, 7, 9.5);"),
        "2024-03-05 14:07:09.5"
    );
    assert_eq!(
        evaluate::<String>("SELECT make_timestamp(2024, 3, 5, 14, 7, 9);"),
        "2024-03-05 14:07:09"
    );
}

#[test]
fn make_date_is_unaffected() {
    assert_eq!(evaluate::<String>("SELECT make_date(2024, 3, 5);"), "2024-03-05");
}

/// PostgreSQL answers NULL when any argument is NULL. `printf` renders a NULL
/// as zero, so every one of these used to answer a plausible wrong string.
#[test]
fn a_null_argument_makes_every_make_function_null() {
    for call in [
        "make_date(2024, NULL, 5)",
        "make_date(NULL, 3, 5)",
        "make_time(8, 15, NULL)",
        "make_time(NULL, 15, 0.5)",
        "make_timestamp(2024, 3, 5, 14, 7, NULL)",
    ] {
        assert_eq!(
            evaluate::<Option<String>>(&format!("SELECT {call};")),
            None,
            "{call} must be NULL"
        );
    }
}

// ---------- the ceiling ----------

/// PostgreSQL answers 1709647629.123456 and 2024-03-05 14:07:09.123456.
/// SQLite's date functions hold milliseconds, so both stop at three decimals.
/// This is the README's caveat, pinned so it cannot go stale unnoticed.
#[test]
fn the_timestamp_paths_stop_at_the_millisecond() {
    let epoch: f64 = evaluate("SELECT extract(epoch from timestamp '2024-03-05 14:07:09.123456');");
    assert!((epoch - 1_709_647_629.123).abs() < 1e-6, "got {epoch}");
    assert_eq!(
        evaluate::<String>("SELECT to_timestamp(1709647629.123456);"),
        "2024-03-05 14:07:09.123"
    );
}

// ---------- the way back ----------

/// The crate now emits a lone `unixepoch(x, 'subsec')`, so it has to read one
/// back. Before this item only the paired form was ever emitted.
#[test]
fn a_lone_subsecond_unixepoch_reverses_to_extract_epoch() {
    let pg = reverse("SELECT unixepoch(ts, 'subsec') FROM t;");
    assert!(pg.contains("EXTRACT(EPOCH FROM ts)"), "{pg}");
}

#[test]
fn a_subsecond_datetime_reverses_to_to_timestamp() {
    let pg = reverse("SELECT datetime(n, 'unixepoch', 'subsec') FROM t;");
    assert!(pg.contains("to_timestamp(n)"), "{pg}");
}

/// The shapes the crate itself emits have to come back, or it can no longer
/// read its own output. This reverses the exact emitted text rather than a
/// hand-written equivalent, so a change to either direction that forgets the
/// other fails here.
#[test]
fn the_crate_reads_back_what_it_emitted() {
    for pg in ["SELECT extract(epoch from ts) FROM t;", "SELECT to_timestamp(n) FROM t;"] {
        let emitted = translate(pg);
        let restored = reverse(&format!("{emitted};"));
        let wanted =
            if pg.contains("epoch from") { "EXTRACT(EPOCH FROM ts)" } else { "to_timestamp(n)" };
        assert!(restored.contains(wanted), "{pg}\n  emitted {emitted}\n  restored {restored}");
    }
}
