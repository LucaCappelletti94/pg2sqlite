//! The hex conversion, whose two halves disagreed on case.
//!
//! Measured against both engines rather than recalled: PostgreSQL's
//! `encode(x, 'hex')` answers lowercase (`0abcde`) and SQLite's `hex(x)`
//! answers uppercase (`0ABCDE`). So neither direction may carry the name across
//! bare, and each has to fold the case back.
//!
//! Before this, the outbound direction refused `encode` outright while advising
//! "Consider using hex()/unhex()", which is what the inbound direction had just
//! rewritten away, so a SQLite hex call could not survive a round trip.
//!
//! PostgreSQL also takes the encoding name in any case and takes a computed
//! argument, both measured, which is why the spelling is matched
//! case-insensitively and a non-literal argument is refused rather than
//! guessed.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

const DDL: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT, bin BYTEA, enc TEXT);";

fn schema() -> ParserDB {
    Pg2Sqlite::default()
        .sql(DDL)
        .expect("fixture parses")
        .build_schema()
        .expect("fixture builds a schema")
}

/// The statement a row's translation adds on top of the translated fixture.
fn forward(postgres: &str) -> Result<String, pg2sqlite::errors::Error> {
    let statements = Pg2Sqlite::default()
        .sql(&format!("{DDL}{postgres};"))?
        .translate_to_sql(&Pg2SqliteOptions::default())?;
    Ok(statements.last().cloned().unwrap_or_default())
}

fn reverse(sqlite: &str) -> Result<String, pg2sqlite::errors::Error> {
    let statements =
        Pg2Sqlite::default().reverse_sql(sqlite, &schema(), &Pg2SqliteOptions::default())?;
    Ok(statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
}

/// Runs a translated expression over a fixture row and answers the one value.
fn value_of(sqlite: &str) -> String {
    let connection = rusqlite::Connection::open_in_memory().expect("open SQLite");
    connection
        .execute_batch(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT, bin BLOB, enc TEXT);
             INSERT INTO t VALUES (1, '0abcde', x'0abcde', 'hex');",
        )
        .expect("fixture");
    connection.query_row(sqlite, [], |row| row.get::<_, String>(0)).expect("one value")
}

// ---------- outbound: PostgreSQL to SQLite ----------

/// PostgreSQL answers lowercase here, so the uppercase `hex` has to be folded.
/// Asserted on the value rather than on the shape, because the case is the
/// whole point.
#[test]
fn encode_hex_keeps_postgres_lowercase() {
    let emitted = forward("SELECT encode(bin, 'hex') FROM t").expect("hex is translatable");
    assert_eq!(value_of(&emitted), "0abcde", "PostgreSQL answers lowercase: {emitted}");
}

#[test]
fn decode_hex_becomes_unhex() {
    let emitted = forward("SELECT encode(decode(s, 'hex'), 'hex') FROM t")
        .expect("both halves are translatable");
    assert_eq!(value_of(&emitted), "0abcde", "got: {emitted}");
}

/// PostgreSQL takes the encoding name in any case, measured, so the match on it
/// cannot be case-sensitive.
#[test]
fn the_encoding_name_is_matched_without_regard_to_case() {
    for spelling in ["HEX", "Hex", "hEx"] {
        let emitted = forward(&format!("SELECT encode(bin, '{spelling}') FROM t"))
            .unwrap_or_else(|error| panic!("{spelling} is the same encoding: {error}"));
        assert_eq!(value_of(&emitted), "0abcde", "{spelling}: {emitted}");
    }
}

/// SQLite has no name for either, so they keep the refusal. Without this the
/// hex arm could quietly answer every encoding.
#[test]
fn the_other_encodings_are_still_refused() {
    for encoding in ["base64", "escape"] {
        for call in [format!("encode(bin, '{encoding}')"), format!("decode(s, '{encoding}')")] {
            let error = forward(&format!("SELECT {call} FROM t"))
                .expect_err("SQLite has no name for this encoding");
            assert!(
                error.to_string().contains(encoding),
                "the refusal should name {encoding}, got: {error}"
            );
        }
    }
}

/// PostgreSQL takes a computed encoding, measured, so this is valid input whose
/// encoding cannot be known at translation time. Guessing hex would be wrong
/// whenever the column held anything else.
#[test]
fn a_computed_encoding_is_refused_rather_than_guessed() {
    let error =
        forward("SELECT encode(bin, enc) FROM t").expect_err("the encoding is not knowable");
    assert!(error.to_string().contains("encode"), "got: {error}");
}

// ---------- inbound: SQLite to PostgreSQL ----------

/// SQLite answers uppercase, so the lowercase `encode` has to be folded back.
/// This half was wrong in the same way the outbound half was missing.
#[test]
fn hex_keeps_sqlite_uppercase() {
    let postgres = reverse("SELECT hex(bin) FROM t").expect("hex reverses");
    Parser::parse_sql(&PostgreSqlDialect {}, &postgres)
        .unwrap_or_else(|error| panic!("`{postgres}` should parse as PostgreSQL: {error}"));
    assert!(
        postgres.to_lowercase().contains("upper("),
        "SQLite hex is uppercase and encode is lowercase: {postgres}"
    );
}

#[test]
fn unhex_becomes_decode() {
    let postgres = reverse("SELECT unhex(s) FROM t").expect("unhex reverses");
    assert!(postgres.contains("decode(s, 'hex')"), "got: {postgres}");
}

/// The whole point of both halves: a SQLite hex call survives the trip and
/// answers what it started with.
#[test]
fn a_hex_call_survives_the_round_trip_unchanged() {
    let original = "SELECT hex(bin) FROM t";
    let postgres = reverse(original).expect("hex reverses");
    let back = forward(postgres.trim_end_matches(';'))
        .expect("what the reverse direction emits must translate back");
    assert_eq!(value_of(&back), value_of(original), "the round trip changed the value: {back}");
}
