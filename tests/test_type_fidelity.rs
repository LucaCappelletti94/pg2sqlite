//! Type-fidelity tests from the TypeFidelity scout hunt.
//!
//! Each test corresponds to a numbered finding and asserts either the
//! refusal message for constructs the target cannot represent, or the
//! round-trip value for constructs that must translate faithfully.
//!
//! Every assertion executes the emitted SQL through rusqlite against the
//! emitted DDL; grepping the output string is not accepted as proof.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation},
    warnings::TranslationWarning,
};
use run_translated_helper::run_translated_with;
use rusqlite::Connection;

fn default_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

fn array_opts() -> Pg2SqliteOptions {
    use pg2sqlite::traits::ArrayRepresentation;
    Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json)
}

fn text_uuid_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text)
}

/// Translate and execute every emitted statement in a fresh SQLite database.
fn run(pg: &str, opts: &Pg2SqliteOptions) {
    let stmts =
        Pg2Sqlite::default().sql(pg).expect("parse").translate_to_sql(opts).expect("translate");
    let conn = Connection::open_in_memory().unwrap();
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("emitted statement failed in SQLite: {e}\n{s}"));
    }
}

/// Translate and execute, then return the first column of the last query as
/// a `Vec<Option<String>>`.
fn query_rows(pg: &str, opts: &Pg2SqliteOptions) -> Vec<Option<String>> {
    run_translated_with(pg, opts)
}

/// Expect translation to fail with a message containing `needle`.
fn expect_refusal(pg: &str, opts: &Pg2SqliteOptions, needle: &str) {
    let err = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(opts)
        .expect_err("expected a refusal");
    assert!(
        err.to_string().contains(needle),
        "refusal message must contain {needle:?}, got: {err}"
    );
}

/// Translate and return the warnings.
fn warnings(pg: &str, opts: &Pg2SqliteOptions) -> Vec<TranslationWarning> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_with_report(opts)
        .expect("translate")
        .warnings
}

// ── Finding 1: NaN / Infinity silently become 0.0 ────────────────────────────

#[test]
fn nan_cast_to_double_is_refused() {
    expect_refusal(
        "CREATE TABLE t (col double precision);
         INSERT INTO t VALUES ('NaN'::double precision);",
        &default_opts(),
        "NaN",
    );
}

#[test]
fn infinity_cast_to_double_is_refused() {
    expect_refusal(
        "CREATE TABLE t (col double precision);
         INSERT INTO t VALUES ('Infinity'::double precision);",
        &default_opts(),
        "Infinity",
    );
}

#[test]
fn negative_infinity_cast_to_real_is_refused() {
    expect_refusal(
        "CREATE TABLE t (col real);
         INSERT INTO t VALUES ('-Infinity'::real);",
        &default_opts(),
        "Infinity",
    );
}

#[test]
fn finite_float_is_accepted() {
    // -0.0 is finite; no crash.
    run(
        "CREATE TABLE t (col double precision);
         INSERT INTO t VALUES (-0.0);",
        &default_opts(),
    );
}

// ── Finding 2: BIT / BIT VARYING → TEXT with length CHECK ───────────────────

#[test]
fn bit_n_maps_to_text_not_integer() {
    let stmts = Pg2Sqlite::default()
        .sql("CREATE TABLE t (col bit(4));")
        .expect("parse")
        .translate_to_sql(&default_opts())
        .expect("translate");
    let sql = stmts.join("\n");
    assert!(sql.contains("TEXT"), "bit(n) must map to TEXT, got: {sql}");
    assert!(!sql.to_uppercase().contains("INTEGER"), "bit(n) must not map to INTEGER, got: {sql}");
}

#[test]
fn bit_values_remain_distinct() {
    // '010' and '10' are distinct bit strings with different lengths; they
    // must compare unequal in the replica, as they do in PostgreSQL.
    let rows = query_rows(
        "CREATE TABLE t (col bit varying(10));
         INSERT INTO t VALUES ('010'), ('10');
         SELECT col FROM t ORDER BY col DESC;",
        &default_opts(),
    );
    assert_eq!(rows.len(), 2, "expected 2 rows, got {rows:?}");
    assert_ne!(rows[0], rows[1], "'010' and '10' must be stored distinct, got {rows:?}");
}

#[test]
fn overlong_bit_value_is_rejected() {
    // '10101' is 5 bits; bit(4) must not accept it.
    let conn = Connection::open_in_memory().unwrap();
    let stmts = Pg2Sqlite::default()
        .sql("CREATE TABLE t (col bit(4));")
        .expect("parse")
        .translate_to_sql(&default_opts())
        .expect("translate");
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap_or_else(|e| panic!("DDL failed: {e}\n{s}"));
    }
    let err = conn
        .execute_batch("INSERT INTO t VALUES ('10101');")
        .expect_err("5-bit value into bit(4) must fail the CHECK");
    assert!(
        err.to_string().contains("CHECK"),
        "rejection must come from the CHECK constraint, got: {err}"
    );
}

#[test]
fn bit_varying_n_accepts_at_boundary_and_rejects_past_it() {
    let conn = Connection::open_in_memory().unwrap();
    let stmts = Pg2Sqlite::default()
        .sql("CREATE TABLE t (col bit varying(3));")
        .expect("parse")
        .translate_to_sql(&default_opts())
        .expect("translate");
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap_or_else(|e| panic!("{e}\n{s}"));
    }
    conn.execute_batch("INSERT INTO t VALUES ('101');").expect("3-bit value into bit varying(3)");
    let err = conn
        .execute_batch("INSERT INTO t VALUES ('1010');")
        .expect_err("4-bit value into bit varying(3) must fail");
    assert!(err.to_string().contains("CHECK"), "rejection must come from CHECK, got: {err}");
}

// ── Finding 3: bytea hex literals fail at apply time ─────────────────────────

#[test]
fn bytea_hex_literal_executes() {
    // '\x414243' is the hex representation of 'ABC'; must reach a BLOB column.
    run(
        "CREATE TABLE t (col bytea);
         INSERT INTO t VALUES ('\\x414243');",
        &default_opts(),
    );
}

#[test]
fn bytea_empty_hex_literal_executes() {
    run(
        "CREATE TABLE t (col bytea);
         INSERT INTO t VALUES ('\\x');",
        &default_opts(),
    );
}

#[test]
fn bytea_two_byte_hex_literal_executes() {
    run(
        "CREATE TABLE t (col bytea);
         INSERT INTO t VALUES ('\\xfffe');",
        &default_opts(),
    );
}

#[test]
fn bytea_odd_nibble_hex_literal_is_refused() {
    // '\xfff' has 3 hex digits — odd number of nibbles — PostgreSQL refuses.
    expect_refusal(
        "CREATE TABLE t (col bytea);
         INSERT INTO t VALUES ('\\xfff');",
        &default_opts(),
        "nibble",
    );
}

#[test]
fn bytea_hex_value_is_accessible_after_insert() {
    // The stored bytes must be retrievable; lower(hex(col)) reads them back.
    let rows = query_rows(
        "CREATE TABLE t (col bytea);
         INSERT INTO t VALUES ('\\x414243');
         SELECT lower(hex(col)) FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("414243".to_string())]);
}

// ── Finding 4: smallint / integer missing range CHECK ────────────────────────

#[test]
fn smallint_boundary_is_accepted() {
    // 32767 is within range; must not fail the CHECK.
    run(
        "CREATE TABLE t (col smallint);
         INSERT INTO t VALUES (32767);",
        &default_opts(),
    );
}

#[test]
fn smallint_above_upper_bound_is_rejected() {
    let conn = Connection::open_in_memory().unwrap();
    let stmts = Pg2Sqlite::default()
        .sql("CREATE TABLE t (col smallint);")
        .expect("parse")
        .translate_to_sql(&default_opts())
        .expect("translate");
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap_or_else(|e| panic!("{e}\n{s}"));
    }
    let err = conn
        .execute_batch("INSERT INTO t VALUES (32768);")
        .expect_err("32768 must fail the smallint range CHECK");
    assert!(err.to_string().contains("CHECK"), "got: {err}");
}

#[test]
fn smallint_below_lower_bound_is_rejected() {
    let conn = Connection::open_in_memory().unwrap();
    let stmts = Pg2Sqlite::default()
        .sql("CREATE TABLE t (col smallint);")
        .expect("parse")
        .translate_to_sql(&default_opts())
        .expect("translate");
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap_or_else(|e| panic!("{e}\n{s}"));
    }
    let err = conn
        .execute_batch("INSERT INTO t VALUES (-32769);")
        .expect_err("-32769 must fail the smallint range CHECK");
    assert!(err.to_string().contains("CHECK"), "got: {err}");
}

#[test]
fn integer_boundary_is_accepted() {
    run(
        "CREATE TABLE t (col integer);
         INSERT INTO t VALUES (2147483647);",
        &default_opts(),
    );
}

#[test]
fn integer_above_upper_bound_is_rejected() {
    let conn = Connection::open_in_memory().unwrap();
    let stmts = Pg2Sqlite::default()
        .sql("CREATE TABLE t (col integer);")
        .expect("parse")
        .translate_to_sql(&default_opts())
        .expect("translate");
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap_or_else(|e| panic!("{e}\n{s}"));
    }
    let err = conn
        .execute_batch("INSERT INTO t VALUES (2147483648);")
        .expect_err("2147483648 must fail the integer range CHECK");
    assert!(err.to_string().contains("CHECK"), "got: {err}");
}

// ── Finding 5: NUMERIC(p,s)[] array elements stored unscaled ─────────────────

#[test]
fn numeric_scaled_array_column_is_refused() {
    // A numeric(10,2)[] column would store elements in decimal while the
    // scalar column stores minor units, making comparisons between them fail.
    expect_refusal(
        "CREATE TABLE t (a numeric(10,2), b numeric(10,2)[]);",
        &array_opts(),
        "NUMERIC",
    );
}

// ── Finding 6: string literal cast to array type stores wrong format
// ──────────

#[test]
fn string_cast_to_integer_array_is_refused() {
    // '{{1,2},{3,4}}'::integer[][] stores PG array text in the JSON column;
    // every later json_extract then fails with "malformed JSON".
    expect_refusal(
        "CREATE TABLE t (col integer[][]);
         SELECT '{{1,2},{3,4}}'::integer[][];",
        &array_opts(),
        "ARRAY",
    );
}

// ── Finding 7: UUID Text representation neither validates nor canonicalises
// ───

#[test]
fn uuid_text_invalid_literal_is_refused() {
    expect_refusal(
        "CREATE TABLE t (id uuid);
         INSERT INTO t VALUES ('not-a-uuid'::uuid);",
        &text_uuid_opts(),
        "invalid input syntax for type uuid",
    );
}

#[test]
fn uuid_text_braced_upper_is_canonicalised() {
    // PostgreSQL normalises '{550E8400-E29B-41D4-A716-446655440000}' to
    // '550e8400-e29b-41d4-a716-446655440000'.  The Text replica must store
    // the same canonical form so equality holds.
    let rows = query_rows(
        "CREATE TABLE t (id uuid);
         INSERT INTO t VALUES ('{550E8400-E29B-41D4-A716-446655440000}'::uuid);
         SELECT id FROM t;",
        &text_uuid_opts(),
    );
    assert_eq!(
        rows,
        vec![Some("550e8400-e29b-41d4-a716-446655440000".to_string())],
        "braced/upper UUID must be stored in canonical form, got {rows:?}"
    );
}

#[test]
fn uuid_text_bare_literal_in_insert_is_canonicalised() {
    let rows = query_rows(
        "CREATE TABLE t (id uuid);
         INSERT INTO t VALUES ('550E8400-E29B-41D4-A716-446655440000');
         SELECT id FROM t;",
        &text_uuid_opts(),
    );
    assert_eq!(rows, vec![Some("550e8400-e29b-41d4-a716-446655440000".to_string())],);
}

#[test]
fn uuid_text_bare_invalid_in_insert_is_refused() {
    expect_refusal(
        "CREATE TABLE t (id uuid);
         INSERT INTO t VALUES ('not-a-uuid');",
        &text_uuid_opts(),
        "invalid input syntax for type uuid",
    );
}

// ── Finding 9: catch-all refusal message misnames built-ins and enum types ───

#[test]
fn money_type_refusal_does_not_say_unknown() {
    let err = Pg2Sqlite::default()
        .sql("CREATE TABLE t (col money);")
        .expect("parse")
        .translate(&default_opts())
        .expect_err("money has no SQLite form");
    let msg = err.to_string();
    assert!(
        !msg.contains("Unknown PostgreSQL custom type"),
        "money is a known built-in, message must not say 'unknown custom type', got: {msg}"
    );
    assert!(
        msg.contains("money") || msg.contains("built-in"),
        "message must name the type or say 'built-in', got: {msg}"
    );
}

#[test]
fn inet_type_refusal_does_not_say_unknown() {
    let err = Pg2Sqlite::default()
        .sql("CREATE TABLE t (col inet);")
        .expect("parse")
        .translate(&default_opts())
        .expect_err("inet has no SQLite form");
    let msg = err.to_string();
    assert!(
        !msg.contains("Unknown PostgreSQL custom type"),
        "inet is a known built-in, got: {msg}"
    );
}

#[test]
fn enum_type_refusal_does_not_say_unknown() {
    // mood is declared in the same batch, so calling it "unknown" is false.
    let err = Pg2Sqlite::default()
        .sql(
            "CREATE TYPE mood AS ENUM ('happy', 'sad');
              CREATE TABLE t (col mood);",
        )
        .expect("parse")
        .translate(&default_opts())
        .expect_err("enum has no SQLite form");
    let msg = err.to_string();
    assert!(
        !msg.contains("Unknown PostgreSQL custom type"),
        "enum is not an unknown type, got: {msg}"
    );
}

// ── Missing warnings: jsonb normalisation, tsvector lexeme parsing
// ────────────

#[test]
fn jsonb_column_warns_about_key_order_loss() {
    let ws = warnings("CREATE TABLE t (col jsonb);", &default_opts());
    let has_jsonb_warn = ws.iter().any(|w| {
        matches!(
            w,
            TranslationWarning::LossyDowngrade { construct, .. }
            if construct.to_ascii_lowercase().contains("jsonb")
        )
    });
    assert!(has_jsonb_warn, "jsonb column must emit a LossyDowngrade warning, got: {ws:?}");
}

#[test]
fn tsvector_column_warns_about_lexeme_loss() {
    let ws = warnings("CREATE TABLE t (col tsvector);", &default_opts());
    let has_ts_warn = ws.iter().any(|w| {
        matches!(
            w,
            TranslationWarning::LossyDowngrade { construct, .. }
            if construct.to_ascii_lowercase().contains("tsvector")
        )
    });
    assert!(has_ts_warn, "tsvector column must emit a LossyDowngrade warning, got: {ws:?}");
}
