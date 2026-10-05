//! Parity for dates, times and zones.
//!
//! A moments table with DATE, TIMESTAMP, TIMESTAMPTZ and INTERVAL columns is
//! seeded with four edge-case rows, then each date/time expression listed
//! below is evaluated on both engines and the results compared.
//!
//! Edge cases in the seed:
//!
//! - Row 1 (id=1): 2024-12-30. Monday in calendar year 2024 but ISO week 1 of
//!   2025 (ISOYEAR != YEAR at year boundaries). Tests EXTRACT(WEEK) and
//!   EXTRACT(ISOYEAR) against EXTRACT(YEAR) for the boundary case.
//! - Row 2 (id=2): 2024-02-29. Last day of February in a leap year (day 60,
//!   month boundary). Tests that leap-day arithmetic produces the right month
//!   and day counts.
//! - Row 3 (id=3): 2024-03-05 14:07:09. Whole-second timestamp for clean
//!   interval arithmetic.
//! - Row 4 (id=4): 2024-03-05 14:07:09.250. Fractional-second timestamp that
//!   exposes the sub-second precision difference between the two engines.
//!
//! Known divergences are listed in KNOWN_DIVERGENCES. Any disagreement not
//! found there causes the test to fail immediately, reporting the expression,
//! the rows, and both outcomes.

use diesel::{
    prelude::*,
    sql_types::{Nullable, Text},
};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

use crate::{helpers, postgres_harness};

// ── Known divergences ────────────────────────────────────────────────────────

/// A construct where the two engines cannot agree, and why.
///
/// Entries are used as allowlists: the test fails for any disagreement whose
/// label does not appear here.
struct Divergence {
    label: &'static str,
    reason: &'static str,
}

/// Expressions that differ between the engines for inherent reasons.
///
/// Entries here appear in comments that explain why the corresponding
/// comparison is written the way it is (e.g. limited to whole-second rows,
/// or using an alternative expression to get consistent text).
const KNOWN_DIVERGENCES: &[Divergence] = &[
    // SQLite's datetime() drops the sub-second component of its first argument.
    // This affects two comparisons:
    //
    //   ts + INTERVAL '1 day' for row 4 (ts = '2024-03-05 14:07:09.250'):
    //     PG gives '2024-03-06 14:07:09.25', SQLite gives '2024-03-06 14:07:09'.
    //
    //   tstz AT TIME ZONE '+05:30' for row 4 (tstz = '2024-03-05 14:07:09.250'):
    //     PG gives '2024-03-05 08:37:09.25', SQLite gives '2024-03-05 08:37:09'.
    //
    // Both comparisons are limited to rows 1-3 (whole-second timestamps) so
    // no runtime failure is triggered. Row 4 is excluded with a comment.
    Divergence {
        label: "datetime_drops_subseconds",
        reason: "datetime() in SQLite drops the sub-second component of its \
                 first argument, PostgreSQL preserves it through interval \
                 arithmetic and AT TIME ZONE",
    },
    // TIMESTAMP AT TIME ZONE returns TIMESTAMPTZ in PostgreSQL. In a UTC
    // session, CAST of TIMESTAMPTZ to TEXT appends a +00 zone suffix. The
    // underlying value is the same instant, but the text representation
    // differs. The comparison for the naive timestamp direction uses
    // EXTRACT(HOUR FROM (...)) instead of the full timestamp text, so the
    // zone suffix never appears in the comparison.
    Divergence {
        label: "naive_ts_at_zone_zone_suffix",
        reason: "TIMESTAMP AT TIME ZONE returns TIMESTAMPTZ in PostgreSQL, \
                 formatted with +00 in a UTC session, but SQLite returns bare \
                 datetime text without a zone marker",
    },
    // Named time zones (e.g. 'America/New_York') have no SQLite equivalent.
    // The translator refuses AT TIME ZONE '<name>' at translation time.
    // This entry records the limitation. No runtime comparison is performed.
    Divergence {
        label: "at_time_zone_named_zone_refused",
        reason: "Named time zones have no SQLite equivalent. The translator \
                 refuses AT TIME ZONE 'America/New_York' and similar at \
                 translation time, so no SQLite execution is possible",
    },
];

// ── Schema ───────────────────────────────────────────────────────────────────

/// PostgreSQL source for the moments table.
const SCHEMA_PG: &str = "
CREATE TABLE moments (
    id   INTEGER PRIMARY KEY,
    d    DATE,
    ts   TIMESTAMP,
    tstz TIMESTAMPTZ,
    ivl  INTERVAL
);";

/// Seed data applied together with the DDL via batch_execute.
///
/// The tstz values are stored without a zone marker so that SQLite's datetime()
/// function (which requires HH:MM format for offsets, not bare +00) can process
/// them. PostgreSQL interprets a TIMESTAMPTZ literal without a zone as UTC in a
/// UTC session, which is the same instant.
///
/// Interval values use day-unit strings ('1 day', '30 days'). PostgreSQL
/// normalizes those to text that matches the stored literal exactly, so the
/// stored-form comparison agrees between the engines.
const SEED_PG: &str = "
INSERT INTO moments VALUES
    (1, '2024-12-30', '2024-12-30 08:30:00',     '2024-12-30 08:30:00',     '1 day'),
    (2, '2024-02-29', '2024-02-29 23:59:59',     '2024-02-29 23:59:59',     '30 days'),
    (3, '2024-03-05', '2024-03-05 14:07:09',     '2024-03-05 14:07:09',     '1 day'),
    (4, '2024-03-05', '2024-03-05 14:07:09.250', '2024-03-05 14:07:09.250', '1 day');
";

/// Typed schema for the SQLite replica.
///
/// After translation every DATE, TIMESTAMP, TIMESTAMPTZ and INTERVAL column
/// becomes TEXT in SQLite, so the SQLite schema uses Text throughout. The
/// typed Diesel DSL handles seed inserts against this schema.
mod schema {
    diesel::table! {
        moments (id) {
            id   -> Integer,
            d    -> Text,
            ts   -> Text,
            tstz -> Text,
            ivl  -> Text,
        }
    }
}

/// A seed row in its SQLite-compatible TEXT form.
#[derive(Insertable)]
#[diesel(table_name = schema::moments)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct MomentRow {
    id: i32,
    d: &'static str,
    ts: &'static str,
    tstz: &'static str,
    ivl: &'static str,
}

fn seed_rows() -> Vec<MomentRow> {
    vec![
        MomentRow {
            id: 1,
            d: "2024-12-30",
            ts: "2024-12-30 08:30:00",
            tstz: "2024-12-30 08:30:00",
            ivl: "1 day",
        },
        MomentRow {
            id: 2,
            d: "2024-02-29",
            ts: "2024-02-29 23:59:59",
            tstz: "2024-02-29 23:59:59",
            ivl: "30 days",
        },
        MomentRow {
            id: 3,
            d: "2024-03-05",
            ts: "2024-03-05 14:07:09",
            tstz: "2024-03-05 14:07:09",
            ivl: "1 day",
        },
        MomentRow {
            id: 4,
            d: "2024-03-05",
            ts: "2024-03-05 14:07:09.250",
            tstz: "2024-03-05 14:07:09.250",
            ivl: "1 day",
        },
    ]
}

// ── Setup ────────────────────────────────────────────────────────────────────

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

/// Opens a fresh PostgreSQL database and applies the schema and seed data.
fn pg_setup() -> PgConnection {
    let mut conn = postgres_harness::fresh_database();
    postgres_harness::apply(&mut conn, &format!("{SCHEMA_PG}{SEED_PG}"))
        .expect("apply datetime schema and seed");
    conn
}

/// Translates the schema and opens a fresh SQLite connection with it applied.
///
/// The typed DSL handles seed inserts because the SQLite schema uses Text for
/// every date/time column, and the string literals in seed_rows() are valid
/// SQLite text values that match what PostgreSQL coerces from the same
/// literals.
fn sqlite_setup() -> SqliteConnection {
    let ddl_stmts = Pg2Sqlite::default()
        .sql(SCHEMA_PG)
        .expect("parse moments DDL")
        .translate(&options())
        .expect("translate moments DDL");

    let mut conn = establish_connection();

    // DDL migration: the typed DSL cannot express CREATE TABLE.
    for stmt in &ddl_stmts {
        diesel::sql_query(stmt.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("translated DDL failed: {e}\n{stmt}"));
    }

    // Seed via the typed Diesel DSL.
    diesel::insert_into(schema::moments::table)
        .values(&seed_rows())
        .execute(&mut conn)
        .expect("seed moments table");

    conn
}

// ── Evaluation ───────────────────────────────────────────────────────────────

/// Result row for expressions wrapped in CAST AS TEXT.
///
/// diesel::sql_query is used for all date/time expression comparisons because
/// EXTRACT, date_trunc and AT TIME ZONE are SQL constructs that cannot be
/// expressed in Diesel's typed DSL. The outer CAST AS TEXT in every query
/// makes the result column a nullable text value on both backends.
#[derive(QueryableByName, Debug)]
struct TextRow {
    #[diesel(sql_type = Nullable<Text>)]
    val: Option<String>,
}

/// Evaluates `SELECT CAST(<pg_expr> AS TEXT) AS val FROM moments WHERE id IN
/// (<ids>) ORDER BY id` on PostgreSQL, returning one value per id.
fn pg_eval(conn: &mut PgConnection, pg_expr: &str, ids: &[i32]) -> Vec<Option<String>> {
    let id_list = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT CAST({pg_expr} AS TEXT) AS val \
         FROM moments WHERE id IN ({id_list}) ORDER BY id"
    );
    // diesel::sql_query is correct: EXTRACT, date_trunc and AT TIME ZONE
    // cannot be expressed in the typed Diesel DSL.
    diesel::sql_query(sql)
        .load::<TextRow>(conn)
        .expect("pg eval")
        .into_iter()
        .map(|r| r.val)
        .collect()
}

/// Translates `SELECT CAST(<pg_expr> AS TEXT) AS val FROM moments WHERE id IN
/// (<ids>) ORDER BY id` through pg2sqlite and runs the result on SQLite.
fn sqlite_eval(conn: &mut SqliteConnection, pg_expr: &str, ids: &[i32]) -> Vec<Option<String>> {
    let id_list = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
    let pg_sql = format!(
        "{SCHEMA_PG}\n\
         SELECT CAST({pg_expr} AS TEXT) AS val \
         FROM moments WHERE id IN ({id_list}) ORDER BY id;"
    );
    let stmts = Pg2Sqlite::default()
        .sql(&pg_sql)
        .expect("parse for sqlite_eval")
        .translate(&options())
        .expect("translate for sqlite_eval");
    let translated = stmts.last().expect("translated SELECT").to_string();
    // diesel::sql_query is correct: the query is dynamically produced by the
    // translator and cannot be expressed as a typed Diesel query.
    diesel::sql_query(&translated)
        .load::<TextRow>(conn)
        .expect("sqlite eval")
        .into_iter()
        .map(|r| r.val)
        .collect()
}

/// All four row ids.
const ALL: &[i32] = &[1, 2, 3, 4];

/// Rows with whole-second timestamps, for expressions where SQLite's datetime()
/// drops sub-second precision (see
/// `KNOWN_DIVERGENCES`, entry `datetime_drops_subseconds`).
const WHOLE_SECOND: &[i32] = &[1, 2, 3];

// ── Test ─────────────────────────────────────────────────────────────────────

/// Every listed date/time expression produces the same result on both engines
/// for the relevant rows, or the divergence appears in KNOWN_DIVERGENCES.
#[test]
fn datetime_expressions_agree() {
    let mut pg = pg_setup();
    let mut sqlite = sqlite_setup();

    let mut unexpected: Vec<String> = Vec::new();
    let mut known_hits: Vec<(&str, String)> = Vec::new();

    // Compare <expr> over <ids>. Records a known divergence or fails.
    let mut cmp = |label: &'static str, expr: &str, ids: &[i32]| {
        let pg_v = pg_eval(&mut pg, expr, ids);
        let sq_v = sqlite_eval(&mut sqlite, expr, ids);
        if pg_v != sq_v {
            let detail = format!("{label}: pg={pg_v:?} sqlite={sq_v:?}");
            if KNOWN_DIVERGENCES.iter().any(|d| d.label == label) {
                known_hits.push((label, detail));
            } else {
                unexpected.push(detail);
            }
        }
    };

    // ── EXTRACT: year, month, day, hour ─────────────────────────────────────
    //
    // These four fields have direct strftime equivalents and agree on all rows.
    // EXTRACT returns numeric in PostgreSQL 14+ but with scale 0 for these
    // integer-valued fields, so CAST AS TEXT gives '2024' not '2024.000000'.

    cmp("extract_year", "EXTRACT(YEAR FROM ts)", ALL);
    cmp("extract_month", "EXTRACT(MONTH FROM d)", ALL);
    cmp("extract_day", "EXTRACT(DAY FROM d)", ALL);
    cmp("extract_hour", "EXTRACT(HOUR FROM ts)", ALL);

    // ── EXTRACT: ISO week and day of week ────────────────────────────────────
    //
    // Row 1 (2024-12-30) is the ISO week boundary: calendar year 2024 but
    // ISO week 1 of 2025. EXTRACT(WEEK) must return 1 and EXTRACT(ISOYEAR)
    // must return 2025, while EXTRACT(YEAR) returns 2024. SQLite uses %V for
    // the ISO week number and %G for the ISO year, both available since 3.30.

    cmp("extract_week", "EXTRACT(WEEK FROM d)", ALL);
    cmp("extract_isoyear", "EXTRACT(ISOYEAR FROM d)", ALL);
    cmp("extract_dow", "EXTRACT(DOW FROM d)", ALL);
    cmp("extract_isodow", "EXTRACT(ISODOW FROM d)", ALL);

    // ── EXTRACT: seconds (compared as milliseconds) ──────────────────────────
    //
    // EXTRACT(SECOND FROM ts) returns numeric with microsecond precision
    // (6 decimal places) in PostgreSQL, giving '9.250000'. SQLite returns a
    // compact REAL (SQLite 3.38+ always includes '.0' for whole-number reals),
    // giving '9.0'. The text representations never agree directly.
    //
    // Instead, both sides multiply by 1000 and round to compare integer
    // milliseconds. Row 4 (9.25s) gives 9250 on both, row 3 (9s) gives 9000.
    // This verifies that fractional seconds are preserved through the
    // translation.

    cmp("extract_second_ms", "CAST(ROUND(EXTRACT(SECOND FROM ts) * 1000) AS INTEGER)", ALL);

    // ── date_trunc ───────────────────────────────────────────────────────────
    //
    // Truncation to day and month. The fractional-second row (id=4) still
    // agrees because truncating to a coarser unit discards the time entirely.

    cmp("date_trunc_day", "date_trunc('day',   ts)", ALL);
    cmp("date_trunc_month", "date_trunc('month', ts)", ALL);

    // ── Interval arithmetic ──────────────────────────────────────────────────
    //
    // Tested on rows 1-3 only (whole-second timestamps). Row 4 has a known
    // divergence (KNOWN_DIVERGENCES["datetime_drops_subseconds"]): SQLite
    // datetime() drops sub-second precision, so '2024-03-05 14:07:09.250' +
    // '1 day' gives '2024-03-06 14:07:09' in SQLite but '2024-03-06
    // 14:07:09.25' in PostgreSQL.

    cmp("ts_add_interval_1_day", "ts + INTERVAL '1 day'", WHOLE_SECOND);
    cmp("ts_sub_interval_1_hour", "ts - INTERVAL '1 hour'", WHOLE_SECOND);

    // ── Difference between two timestamps (integer seconds) ──────────────────
    //
    // EXTRACT(EPOCH FROM (ts - TIMESTAMP '...')) returns numeric with 6dp in
    // PostgreSQL. The outer CAST AS INTEGER truncates to whole seconds, which
    // gives the same text on both engines ('31480200', '5580429', etc.).
    //
    // The sub-second fractional part is already verified by the
    // extract_second_ms test above, which uses ROUND rather than truncation.

    cmp(
        "epoch_diff",
        "CAST(EXTRACT(EPOCH FROM (ts - TIMESTAMP '2024-01-01 00:00:00')) AS INTEGER)",
        ALL,
    );

    // ── AT TIME ZONE over TIMESTAMPTZ ────────────────────────────────────────
    //
    // TIMESTAMPTZ AT TIME ZONE returns TIMESTAMP in PostgreSQL (no zone marker
    // in the text form), so CAST AS TEXT produces '2024-12-30 03:00:00' on
    // both sides. The translator negates the offset because a zone-aware
    // timestamp is already UTC and converting to a local zone subtracts the
    // offset.
    //
    // Tested on rows 1-3 (whole-second tstz values). Row 4 has a known
    // divergence (KNOWN_DIVERGENCES["datetime_drops_subseconds"]): SQLite
    // datetime() drops sub-second precision, so tstz = '2024-03-05
    // 14:07:09.250' would produce '2024-03-05 08:37:09.25' in PostgreSQL
    // but '2024-03-05 08:37:09' in SQLite.

    cmp("tstz_at_time_zone", "tstz AT TIME ZONE '+05:30'", WHOLE_SECOND);

    // ── AT TIME ZONE over naive TIMESTAMP ────────────────────────────────────
    //
    // TIMESTAMP AT TIME ZONE returns TIMESTAMPTZ in PostgreSQL, which CAST AS
    // TEXT appends +00 in a UTC session. The underlying instant is the same,
    // but the text form is not (see
    // KNOWN_DIVERGENCES["naive_ts_at_zone_zone_suffix"]).
    //
    // To avoid the zone-suffix divergence while still pinning the correct
    // value, the comparison extracts the hour component, which is an integer on
    // both sides. Row 4 (fractional seconds): adding 5h30m to 14:07:09.250
    // gives 19:37:09.250, whose hour is 19 on both engines regardless of the
    // sub-second component being dropped by datetime().

    cmp("ts_at_time_zone_hour", "EXTRACT(HOUR FROM (ts AT TIME ZONE '+05:30'))", ALL);

    // ── Date rendered as text ────────────────────────────────────────────────
    //
    // A DATE column in PostgreSQL formats as 'YYYY-MM-DD' when cast to text.
    // The SQLite column stores the literal string, so the cast is a no-op.

    cmp("date_as_text", "d", ALL);

    // ── INTERVAL column stored form ──────────────────────────────────────────
    //
    // The seed uses '1 day' and '30 days'. PostgreSQL normalizes those interval
    // values to the same text ('1 day', '30 days'), matching SQLite's verbatim
    // text. Hour-based intervals would diverge because PostgreSQL normalizes
    // '1 hour' to '01:00:00', which the test data avoids.

    cmp("ivl_stored", "ivl", ALL);

    // ── Summary ──────────────────────────────────────────────────────────────

    for (label, detail) in &known_hits {
        let reason = KNOWN_DIVERGENCES
            .iter()
            .find(|d| d.label == *label)
            .map_or("reason not found", |d| d.reason);
        eprintln!("known divergence [{label}]: {reason} -- {detail}");
    }

    assert!(
        unexpected.is_empty(),
        "unexpected divergences ({} found):\n{}",
        unexpected.len(),
        unexpected.join("\n")
    );
}

/// The ISO `to_char` codes agree after translation. Row 1 (2024-12-30) is the
/// boundary case: calendar year 2024 but ISO 2025-W01, day 1.
#[test]
fn to_char_iso_codes_agree() {
    let mut pg = pg_setup();
    let mut sqlite = sqlite_setup();

    let expr = "to_char(d, 'IYYY-IW-ID')";
    assert_eq!(
        sqlite_eval(&mut sqlite, expr, ALL),
        pg_eval(&mut pg, expr, ALL),
        "ISO to_char codes must agree after translation"
    );
}
