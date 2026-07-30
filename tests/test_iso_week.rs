//! ISO week numbering for `date_part('week', ...)` and `EXTRACT(WEEK ...)`.
//!
//! PostgreSQL numbers weeks by ISO 8601, Monday based, with week 1 holding the
//! first Thursday. SQLite's `%W` is Sunday based and disagrees at every year
//! boundary. Its `%V` is the ISO one, available since 3.30.0 and well under the
//! 3.46.0 floor.
//!
//! Measured on PostgreSQL 16 and SQLite 3.51.1:
//!
//! | date | `%W` | `%V` = PostgreSQL week | `%G` = ISOYEAR | `%u` = ISODOW |
//! |---|---|---|---|---|
//! | 2023-01-01 | 0 | 52 | 2022 | 7 |
//! | 2021-01-01 | 0 | 53 | 2020 | 5 |
//! | 2024-12-30 | 53 | 1 | 2025 | 1 |
//! | 2020-12-31 | 52 | 53 | 2020 | 4 |
//! | 2024-03-15 | 11 | 11 | 2024 | 5 |

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::Pg2SqliteOptions;
use run_translated_helper::run_translated_with;

/// The three dates the item names, plus one more year boundary and one ordinary
/// date. The first four are where ISO and calendar weeks disagree.
const DATES: [&str; 5] = ["2023-01-01", "2021-01-01", "2024-12-30", "2020-12-31", "2024-03-15"];

fn evaluate(expression: &str) -> Vec<Option<String>> {
    let rows = DATES
        .iter()
        .enumerate()
        .map(|(index, date)| format!("({}, '{date}')", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    run_translated_with(
        &format!(
            "CREATE TABLE t (id INT PRIMARY KEY, d TEXT);
             INSERT INTO t VALUES {rows};
             SELECT {expression} FROM t ORDER BY id;"
        ),
        &Pg2SqliteOptions::default(),
    )
}

fn expected(values: [&str; 5]) -> Vec<Option<String>> {
    values.iter().map(|value| Some((*value).to_string())).collect()
}

#[test]
fn date_part_week_is_the_iso_week() {
    assert_eq!(evaluate("date_part('week', d)"), expected(["52", "53", "1", "53", "11"]));
}

/// The same field through the other syntax. These were separate code paths, one
/// emitting the Sunday based number and the other refusing outright.
#[test]
fn extract_week_agrees_with_date_part() {
    assert_eq!(evaluate("EXTRACT(WEEK FROM d)"), evaluate("date_part('week', d)"));
}

/// An ISO week number means nothing without its ISO year: 2023-01-01 is week 52
/// of 2022, not of 2023.
#[test]
fn extract_isoyear_is_the_iso_year() {
    assert_eq!(
        evaluate("EXTRACT(ISOYEAR FROM d)"),
        expected(["2022", "2020", "2025", "2020", "2024"])
    );
}

/// ISODOW counts Monday as 1 through Sunday as 7, where DOW counts Sunday as 0.
/// 2023-01-01 is a Sunday, so the two disagree on it.
#[test]
fn extract_isodow_counts_from_monday() {
    assert_eq!(evaluate("EXTRACT(ISODOW FROM d)"), expected(["7", "5", "1", "4", "5"]));
    assert_eq!(evaluate("EXTRACT(DOW FROM d)"), expected(["0", "5", "1", "4", "5"]));
}
