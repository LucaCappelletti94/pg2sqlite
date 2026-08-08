//! Case folding stops at ASCII on the SQLite side, and the README says so.
//!
//! PostgreSQL folds by the database collation, which under a UTF-8 locale
//! covers the whole of Unicode, so `ILIKE`, `lower` and `upper` answer
//! differently once the text is not ASCII. Nothing in the translator can close
//! that: SQLite's own `lower` leaves a non-ASCII code point alone, and only an
//! ICU-enabled build changes it.
//!
//! These tests pin the divergence rather than a fix, so they fail the day the
//! answers converge, which is the day the README section has to be rewritten.
//! Every PostgreSQL answer named below was read off PostgreSQL 17 under
//! `en_US.utf8`, and under the `C` collation PostgreSQL answers what SQLite
//! answers.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

/// The last statement of a translation, which is the probe.
fn translate(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .pop()
        .expect("a probe statement")
}

/// The first column of the first row of the emitted probe.
fn evaluate<T: rusqlite::types::FromSql>(pg: &str) -> T {
    let probe = translate(pg);
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    connection
        .query_row(&probe, [], |row| row.get::<_, T>(0))
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"))
}

/// The shape the README shows. Both halves are lowered and the escape is the
/// one PostgreSQL applies by default.
#[test]
fn ilike_lowers_both_sides_and_carries_the_default_escape() {
    assert_eq!(
        translate("SELECT 'ÄBC' ILIKE 'äbc';"),
        "SELECT lower('ÄBC') LIKE lower('äbc') ESCAPE '\\'"
    );
}

#[test]
fn an_ascii_ilike_matches_as_postgresql_does() {
    assert_eq!(evaluate::<i64>("SELECT 'ABC' ILIKE 'abc';"), 1);
}

/// PostgreSQL answers true. The translated form answers false, because
/// `lower('ÄBC')` is `Äbc` in SQLite.
#[test]
fn a_non_ascii_ilike_does_not_match_where_postgresql_does() {
    assert_eq!(evaluate::<i64>("SELECT 'ÄBC' ILIKE 'äbc';"), 0);
    assert_eq!(evaluate::<i64>("SELECT 'ΣΙΓΜΑ' ILIKE 'σιγμα';"), 0);
}

/// PostgreSQL answers `äbc`, `ÄBC` and `σιγμα` for these three.
#[test]
fn lower_and_upper_leave_a_non_ascii_letter_alone() {
    assert_eq!(evaluate::<String>("SELECT lower('ÄBC');"), "Äbc");
    assert_eq!(evaluate::<String>("SELECT upper('äbc');"), "äBC");
    assert_eq!(evaluate::<String>("SELECT lower('ΣΙΓΜΑ');"), "ΣΙΓΜΑ");
}

#[test]
fn ascii_folding_itself_is_unaffected() {
    assert_eq!(evaluate::<String>("SELECT lower('ABC');"), "abc");
    assert_eq!(evaluate::<String>("SELECT upper('abc');"), "ABC");
}

/// A plain `LIKE` stays case-sensitive, which is what the emitted
/// `PRAGMA case_sensitive_like` is for and what PostgreSQL does. Only the
/// pragma-carrying translation is executed here, so the assertion covers the
/// pragma too.
#[test]
fn a_plain_like_is_case_sensitive_on_both_engines() {
    let statements = Pg2Sqlite::default()
        .sql("SELECT 'ABC' LIKE 'abc';")
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate");
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    let mut answer = None;
    for statement in &statements {
        if statement.starts_with("PRAGMA") {
            connection.execute_batch(&format!("{statement};")).expect("apply the pragma");
        } else {
            answer = Some(
                connection
                    .query_row(statement, [], |row| row.get::<_, i64>(0))
                    .expect("run the probe"),
            );
        }
    }
    assert_eq!(answer, Some(0), "{statements:?}");
}
