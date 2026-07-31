//! SQLite's `LIMIT offset, count` form in the reverse direction.
//!
//! PostgreSQL has no comma form, so it has to become `LIMIT count OFFSET
//! offset`. The operands are in the opposite order to the spelling, which is
//! the trap: `LIMIT 5, 10` is offset 5 and limit 10. Measured on both, it
//! returns rows 6 through 10 of ten, as does `LIMIT 10 OFFSET 5`.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY);";

fn reverse(sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(SCHEMA).expect("parse");
    let schema = translator.build_schema().expect("schema");
    translator
        .reverse_sql(sqlite_sql, &schema, &Pg2SqliteOptions::default())
        .expect("reverse")
        .first()
        .expect("one statement")
        .to_string()
}

#[test]
fn the_comma_form_becomes_limit_offset() {
    let pg = reverse("SELECT id FROM t ORDER BY id LIMIT 5, 10");
    assert!(!pg.contains("5, 10"), "PostgreSQL has no comma form: {pg}");
    assert!(pg.contains("LIMIT 10"), "10 is the count: {pg}");
    assert!(pg.contains("OFFSET 5"), "5 is the offset: {pg}");
}

/// The explicit spelling already reverses and must be untouched, which is what
/// catches a fix that transposes the operands for both forms.
#[test]
fn the_explicit_form_is_unchanged() {
    let pg = reverse("SELECT id FROM t ORDER BY id LIMIT 10 OFFSET 5");
    assert!(pg.contains("LIMIT 10"), "{pg}");
    assert!(pg.contains("OFFSET 5"), "{pg}");
}

/// Both numbers survive a round trip, which the comma form loses if either
/// direction transposes them.
#[test]
fn a_round_trip_preserves_both_numbers() {
    let pg = reverse("SELECT id FROM t ORDER BY id LIMIT 5, 10");
    let back = Pg2Sqlite::default()
        .sql(&format!("{SCHEMA}{pg};"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("forward")
        .join("\n");
    assert!(back.contains("LIMIT 10"), "{back}");
    assert!(back.contains("OFFSET 5"), "{back}");
}
