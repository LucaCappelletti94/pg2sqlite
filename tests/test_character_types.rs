//! PostgreSQL character types, which SQLite stores as TEXT.
//!
//! Measured on PostgreSQL 16 before implementing. A value longer than the
//! declared length is refused with `value too long for type character
//! varying(5)`, so the length is enforced and not a hint. `CHAR(3)` holding
//! `'a'` occupies three octets while `length()` reports 1, which is the blank
//! padding SQLite cannot reproduce. Bare `CHAR` is `character` with a maximum
//! length of 1. `VARCHAR(5 OCTETS)` is a syntax error, so the octet unit
//! cannot arrive from PostgreSQL.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    warnings::TranslationWarning,
};

/// Translates `pg` and applies the emitted DDL to a fresh database.
fn apply(pg: &str) -> SqliteConnection {
    let statements = Pg2Sqlite::default()
        .sql(pg)
        .expect("script should parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .unwrap_or_else(|error| panic!("script should translate: {error}"));

    let mut connection =
        SqliteConnection::establish(":memory:").expect("in-memory SQLite should open");
    for statement in &statements {
        // Emitted DDL is the artifact under test, so it runs as text.
        diesel::sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted DDL failed: {error}\n{statement}"));
    }
    connection
}

#[derive(diesel::QueryableByName)]
struct ColumnInfo {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    declared: String,
}

/// The declared SQLite type of every column of `table` but `id`.
fn declared_types(connection: &mut SqliteConnection, table: &str) -> Vec<(String, String)> {
    // A pragma has no diesel DSL form, so the column types are read as text.
    diesel::sql_query(format!(
        "SELECT name, type AS declared FROM pragma_table_info('{table}') WHERE name <> 'id'"
    ))
    .load::<ColumnInfo>(connection)
    .expect("pragma should read")
    .into_iter()
    .map(|column| (column.name, column.declared))
    .collect()
}

#[test]
fn every_character_spelling_is_stored_as_text() {
    let mut connection = apply(
        "CREATE TABLE names (
             id INT PRIMARY KEY,
             a CHAR,
             b CHAR(3),
             c CHARACTER,
             d CHARACTER(3),
             e VARCHAR,
             f VARCHAR(5),
             g CHARACTER VARYING,
             h CHARACTER VARYING(9)
         );",
    );

    let declared = declared_types(&mut connection, "names");
    assert_eq!(declared.len(), 8, "every column should reach the table");
    for (name, declared) in declared {
        assert_eq!(declared, "TEXT", "{name} should be stored as TEXT");
    }
}

/// PostgreSQL refuses a value longer than the declared length, so the emitted
/// column carries a bound rather than dropping the limit.
#[test]
fn a_declared_length_is_enforced_by_the_emitted_column() {
    let mut connection =
        apply("CREATE TABLE names (id INT PRIMARY KEY, short VARCHAR(5), padded CHAR(3));");

    let fits = diesel::sql_query("INSERT INTO names (id, short) VALUES (1, 'abcde')")
        .execute(&mut connection);
    assert!(fits.is_ok(), "a value of exactly the declared length fits: {fits:?}");

    let overflows = diesel::sql_query("INSERT INTO names (id, short) VALUES (2, 'abcdef')")
        .execute(&mut connection);
    assert!(overflows.is_err(), "a longer value must be refused, as PostgreSQL refuses it");

    let padded_overflows = diesel::sql_query("INSERT INTO names (id, padded) VALUES (3, 'abcd')")
        .execute(&mut connection);
    assert!(padded_overflows.is_err(), "the same bound applies to a blank padded column");
}

/// A length is a bound, not a requirement, so a shorter value is accepted.
#[test]
fn a_shorter_value_is_still_accepted() {
    let mut connection = apply("CREATE TABLE names (id INT PRIMARY KEY, short VARCHAR(5));");

    let inserted = diesel::sql_query("INSERT INTO names (id, short) VALUES (1, 'ab')")
        .execute(&mut connection);
    assert!(inserted.is_ok(), "a shorter value fits: {inserted:?}");
}

/// Bare `VARCHAR` has no declared length, so there is no bound to enforce and
/// any value fits. This passes before the change too.
#[test]
fn an_undeclared_length_bounds_nothing() {
    let mut connection = apply("CREATE TABLE names (id INT PRIMARY KEY, any_length VARCHAR);");

    let inserted =
        diesel::sql_query("INSERT INTO names (id, any_length) VALUES (1, 'as long as you like')")
            .execute(&mut connection);
    assert!(inserted.is_ok(), "an unbounded column takes any length: {inserted:?}");
}

/// The warnings a translation of `pg` records.
fn warnings(pg: &str) -> Vec<TranslationWarning> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("script should parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("script should translate")
        .warnings
}

/// SQLite pads nothing, so `CHAR(n)` loses the blank padding PostgreSQL
/// applies. That is reported rather than dropped in silence.
#[test]
fn blank_padding_loss_is_reported_for_every_padded_spelling() {
    for spelling in ["CHAR", "CHAR(3)", "CHARACTER", "CHARACTER(3)"] {
        let reported =
            warnings(&format!("CREATE TABLE names (id INT PRIMARY KEY, padded {spelling});"));
        assert!(
            reported.iter().any(|warning| {
                matches!(
                    warning,
                    TranslationWarning::LossyDowngrade { location, .. } if location == "padded"
                )
            }),
            "{spelling} should report the padding loss on the column, got {reported:?}"
        );
    }
}

/// Guards the report from firing on a type that is not blank padded. Only
/// `CHAR` pads, so `VARCHAR` and `CHARACTER VARYING` must stay quiet, and
/// dropping a length nobody declared is not a loss either.
#[test]
fn a_varying_length_reports_no_padding_loss() {
    for spelling in ["VARCHAR", "VARCHAR(5)", "CHARACTER VARYING", "CHARACTER VARYING(9)", "TEXT"] {
        let reported =
            warnings(&format!("CREATE TABLE names (id INT PRIMARY KEY, varying {spelling});"));
        assert!(
            !reported
                .iter()
                .any(|warning| matches!(warning, TranslationWarning::LossyDowngrade { .. })),
            "{spelling} is not blank padded and must not report a downgrade, got {reported:?}"
        );
    }
}
