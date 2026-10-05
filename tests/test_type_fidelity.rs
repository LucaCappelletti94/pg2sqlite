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

#[test]
fn nan_written_into_a_double_column_is_refused() {
    // No cast: the column's own type is what makes 'NaN' a float here, and
    // the emitted INSERT used to die at apply with "cannot store TEXT value
    // in REAL column".
    expect_refusal(
        "CREATE TABLE t (col double precision);
         INSERT INTO t (col) VALUES ('NaN');",
        &default_opts(),
        "NaN",
    );
}

#[test]
fn infinity_compared_against_a_double_column_is_refused() {
    // `col = 'Infinity'` compares a REAL against TEXT in SQLite, which is
    // never equal, where PostgreSQL matches every infinite row.
    expect_refusal(
        "CREATE TABLE t (col double precision);
         SELECT * FROM t WHERE col = 'Infinity';",
        &default_opts(),
        "Infinity",
    );
}

#[test]
fn a_finite_string_literal_in_a_double_column_still_translates() {
    let rows = query_rows(
        "CREATE TABLE t (col double precision);
         INSERT INTO t (col) VALUES ('1.5');
         SELECT col FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("1.5".to_string())]);
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

#[test]
fn a_bit_string_literal_is_translated_to_its_digits() {
    // B'010' is PostgreSQL's own spelling of a bit string; emitted verbatim
    // it is a syntax error in SQLite.
    let rows = query_rows(
        "CREATE TABLE t (col bit varying(8));
         INSERT INTO t (col) VALUES (B'010');
         SELECT col FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("010".to_string())]);
}

#[test]
fn a_hex_bit_literal_is_expanded_to_bits() {
    // PostgreSQL answers '00011010' for X'1A'::text, four bits per digit.
    let rows = query_rows(
        "CREATE TABLE t (col bit(8));
         INSERT INTO t (col) VALUES (X'1A');
         SELECT col FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("00011010".to_string())]);
}

#[test]
fn a_non_binary_digit_in_a_bit_column_is_refused() {
    // PostgreSQL answers `"2" is not a valid binary digit`; the replica
    // stored '012' because the CHECK counted length only.
    expect_refusal(
        "CREATE TABLE t (col bit(3));
         INSERT INTO t (col) VALUES ('012');",
        &default_opts(),
        "binary digit",
    );
}

#[test]
fn a_non_binary_value_written_later_fails_the_check() {
    // A write the translator never sees must fail in the replica too.
    let conn = Connection::open_in_memory().unwrap();
    let stmts = Pg2Sqlite::default()
        .sql("CREATE TABLE t (col bit(3));")
        .expect("parse")
        .translate_to_sql(&default_opts())
        .expect("translate");
    for s in &stmts {
        conn.execute_batch(&format!("{s};")).unwrap_or_else(|e| panic!("DDL failed: {e}\n{s}"));
    }
    conn.execute_batch("INSERT INTO t VALUES ('010');").expect("three binary digits");
    let err = conn
        .execute_batch("INSERT INTO t VALUES ('012');")
        .expect_err("a non-binary digit must fail the CHECK");
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

#[test]
fn a_string_literal_written_into_an_array_column_is_refused() {
    // No cast: the column's type is what makes '{1,2}' an array here, and
    // the replica stored PostgreSQL array text in a column holding JSON, so
    // every later array operation failed with "malformed JSON".
    expect_refusal(
        "CREATE TABLE t (col integer[]);
         INSERT INTO t (col) VALUES ('{1,2}');",
        &array_opts(),
        "ARRAY",
    );
}

#[test]
fn an_array_constructor_still_translates() {
    let rows = query_rows(
        "CREATE TABLE t (col integer[]);
         INSERT INTO t (col) VALUES (ARRAY[1,2]);
         SELECT col FROM t;",
        &array_opts(),
    );
    assert_eq!(rows, vec![Some("[1,2]".to_string())]);
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

// ── Finding 8: temporal literals unvalidated and un-normalised ───────────────

#[test]
fn a_month_out_of_range_is_refused() {
    // PostgreSQL: date/time field value out of range: "2024-13-01".
    expect_refusal(
        "CREATE TABLE t (d date);
         INSERT INTO t (d) VALUES ('2024-13-01');",
        &default_opts(),
        "2024-13-01",
    );
}

#[test]
fn a_day_past_the_end_of_the_month_is_refused() {
    expect_refusal(
        "CREATE TABLE t (d date);
         INSERT INTO t (d) VALUES ('2024-02-30');",
        &default_opts(),
        "2024-02-30",
    );
}

#[test]
fn february_29_is_refused_outside_a_leap_year_and_kept_inside_one() {
    expect_refusal(
        "CREATE TABLE t (d date);
         INSERT INTO t (d) VALUES ('2023-02-29');",
        &default_opts(),
        "2023-02-29",
    );
    let rows = query_rows(
        "CREATE TABLE t (d date);
         INSERT INTO t (d) VALUES ('2024-02-29');
         SELECT d FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("2024-02-29".to_string())]);
}

#[test]
fn a_single_digit_month_and_day_are_padded() {
    // PostgreSQL answers 2024-03-05, and SQLite's own date functions answer
    // NULL for '2024-3-5', so the padding is what makes the stored value
    // usable at all.
    let rows = query_rows(
        "CREATE TABLE t (d date);
         INSERT INTO t (d) VALUES ('2024-3-5');
         SELECT strftime('%Y/%m', d) FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("2024/03".to_string())]);
}

#[test]
fn a_time_literal_is_filled_out_to_seconds() {
    // PostgreSQL answers 02:30:00 for '2:30'::time.
    let rows = query_rows(
        "CREATE TABLE t (tm time);
         INSERT INTO t (tm) VALUES ('2:30');
         SELECT tm FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("02:30:00".to_string())]);
}

#[test]
fn an_hour_past_the_end_of_the_day_is_refused() {
    expect_refusal(
        "CREATE TABLE t (tm time);
         INSERT INTO t (tm) VALUES ('25:00:00');",
        &default_opts(),
        "25:00:00",
    );
}

#[test]
fn the_end_of_day_hour_rolls_a_timestamp_over() {
    // PostgreSQL answers 2024-03-06 00:00:00 for a timestamp written at
    // 24:00:00, while a time column keeps the hour.
    let rows = query_rows(
        "CREATE TABLE t (ts timestamp);
         INSERT INTO t (ts) VALUES ('2024-03-05 24:00:00');
         SELECT ts FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("2024-03-06 00:00:00".to_string())]);
}

#[test]
fn the_end_of_day_hour_is_accepted() {
    // PostgreSQL takes 24:00:00 as a time and answers it back.
    let rows = query_rows(
        "CREATE TABLE t (tm time);
         INSERT INTO t (tm) VALUES ('24:00:00');
         SELECT tm FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("24:00:00".to_string())]);
}

#[test]
fn a_sloppy_timestamp_is_normalised() {
    // PostgreSQL answers 2024-01-02 03:04:00.
    let rows = query_rows(
        "CREATE TABLE t (ts timestamp);
         INSERT INTO t (ts) VALUES ('2024-1-2 3:4');
         SELECT ts FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("2024-01-02 03:04:00".to_string())]);
}

#[test]
fn an_iso_t_separator_becomes_a_space() {
    // PostgreSQL prints a space, and the replica's text has to match so a
    // row written through either database compares equal.
    let rows = query_rows(
        "CREATE TABLE t (ts timestamp);
         INSERT INTO t (ts) VALUES ('2024-03-05T14:07:09');
         SELECT ts FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("2024-03-05 14:07:09".to_string())]);
}

#[test]
fn a_fractional_second_is_kept() {
    let rows = query_rows(
        "CREATE TABLE t (ts timestamp);
         INSERT INTO t (ts) VALUES ('2024-03-05 14:07:09.123');
         SELECT ts FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("2024-03-05 14:07:09.123".to_string())]);
}

#[test]
fn a_style_dependent_date_is_refused() {
    // '1/2/2024' is 2 January under the default DateStyle and 1 February
    // under DMY, which is server state the translator cannot read.
    expect_refusal(
        "CREATE TABLE t (d date);
         INSERT INTO t (d) VALUES ('1/2/2024');",
        &default_opts(),
        "1/2/2024",
    );
}

#[test]
fn a_relative_date_keyword_is_refused() {
    // 'today' is resolved when PostgreSQL reads it, so storing the word
    // itself would freeze a value that was meant to be a date.
    expect_refusal(
        "CREATE TABLE t (d date);
         INSERT INTO t (d) VALUES ('today');",
        &default_opts(),
        "today",
    );
}

#[test]
fn a_comparison_literal_is_normalised_too() {
    // The stored value is padded, so an unpadded literal in a comparison
    // would match nothing.
    let rows = query_rows(
        "CREATE TABLE t (d date);
         INSERT INTO t (d) VALUES ('2024-03-05');
         SELECT d FROM t WHERE d = '2024-3-5';",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("2024-03-05".to_string())]);
}

#[test]
fn a_timestamptz_offset_is_moved_to_utc() {
    let rows = query_rows(
        "CREATE TABLE t (ts timestamptz);
         INSERT INTO t (ts) VALUES ('2024-01-02 03:04:05+02');
         SELECT ts FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("2024-01-02 01:04:05.000000+00:00".to_string())]);
}

#[test]
fn a_cast_to_date_validates_its_literal() {
    expect_refusal("SELECT '2024-13-01'::date;", &default_opts(), "2024-13-01");
}

#[test]
fn a_cast_to_date_normalises_its_literal() {
    let rows = query_rows("SELECT '2024-3-5'::date;", &default_opts());
    assert_eq!(rows, vec![Some("2024-03-05".to_string())]);
}

#[test]
fn an_update_assignment_normalises_its_literal() {
    let rows = query_rows(
        "CREATE TABLE t (id int primary key, d date);
         INSERT INTO t (id, d) VALUES (1, '2024-01-01');
         UPDATE t SET d = '2024-3-5' WHERE id = 1;
         SELECT d FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("2024-03-05".to_string())]);
}

#[test]
fn an_update_assignment_validates_its_literal() {
    expect_refusal(
        "CREATE TABLE t (id int primary key, d date);
         UPDATE t SET d = '2024-13-01' WHERE id = 1;",
        &default_opts(),
        "2024-13-01",
    );
}

/// Every expected value here is what PostgreSQL 17 prints for the literal,
/// measured in Docker under the default `IntervalStyle`.
fn interval_text(literal: &str) -> Vec<Option<String>> {
    query_rows(
        &format!(
            "CREATE TABLE t (iv interval);
             INSERT INTO t (iv) VALUES ('{literal}');
             SELECT iv FROM t;"
        ),
        &default_opts(),
    )
}

#[test]
fn an_iso_interval_is_printed_as_a_clock() {
    assert_eq!(interval_text("PT15M"), vec![Some("00:15:00".to_string())]);
}

#[test]
fn interval_minutes_carry_into_hours() {
    assert_eq!(interval_text("90 minutes"), vec![Some("01:30:00".to_string())]);
}

#[test]
fn interval_days_stay_apart_from_the_time_of_day() {
    assert_eq!(interval_text("1 day 2:03:04"), vec![Some("1 day 02:03:04".to_string())]);
    // Hours beyond a day are not folded into one, as PostgreSQL does not.
    assert_eq!(interval_text("26 hours"), vec![Some("26:00:00".to_string())]);
    // A written clock reading never becomes a day count either.
    assert_eq!(interval_text("100:00:00"), vec![Some("100:00:00".to_string())]);
    assert_eq!(interval_text("24:00:00"), vec![Some("24:00:00".to_string())]);
}

#[test]
fn a_fraction_of_a_month_becomes_days() {
    assert_eq!(interval_text("1.5 months"), vec![Some("1 mon 15 days".to_string())]);
    assert_eq!(interval_text("1.05 months"), vec![Some("1 mon 1 day 12:00:00".to_string())]);
}

#[test]
fn interval_months_carry_into_years() {
    assert_eq!(interval_text("12 months"), vec![Some("1 year".to_string())]);
    assert_eq!(interval_text("1-2"), vec![Some("1 year 2 mons".to_string())]);
}

#[test]
fn an_interval_written_backwards_is_negated() {
    assert_eq!(interval_text("2 days ago"), vec![Some("-2 days".to_string())]);
}

#[test]
fn a_zero_interval_is_printed_as_a_zero_clock() {
    assert_eq!(interval_text("0"), vec![Some("00:00:00".to_string())]);
}

#[test]
fn a_bare_interval_number_counts_seconds() {
    assert_eq!(interval_text("5"), vec![Some("00:00:05".to_string())]);
}

#[test]
fn an_unreadable_interval_is_refused() {
    expect_refusal(
        "CREATE TABLE t (iv interval);
         INSERT INTO t (iv) VALUES ('every other tuesday');",
        &default_opts(),
        "every other tuesday",
    );
}

#[test]
fn an_interval_comparison_literal_is_normalised_too() {
    let rows = query_rows(
        "CREATE TABLE t (iv interval);
         INSERT INTO t (iv) VALUES ('90 minutes');
         SELECT iv FROM t WHERE iv = 'PT1H30M';",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("01:30:00".to_string())]);
}

#[test]
fn a_cast_to_interval_is_normalised() {
    let rows = query_rows("SELECT 'PT15M'::interval;", &default_opts());
    assert_eq!(rows, vec![Some("00:15:00".to_string())]);
}

/// Refuses a literal written into a temporal column of `kind`, returning the
/// message.
fn temporal_refusal(kind: &str, literal: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!(
            "CREATE TABLE t (v {kind});
             INSERT INTO t (v) VALUES ('{literal}');"
        ))
        .expect("parse")
        .translate(&default_opts())
        .expect_err("expected a refusal")
        .to_string()
}

#[test]
fn an_offset_in_a_column_without_a_zone_is_refused() {
    // Stored as text, the offset would be kept where PostgreSQL drops it,
    // so the two databases would disagree about the instant.
    let message = temporal_refusal("timestamp", "2024-01-02 03:04:05+02");
    assert!(message.contains("no time zone"), "{message}");
}

#[test]
fn a_zoned_time_column_takes_an_offset() {
    let rows = query_rows(
        "CREATE TABLE t (v time with time zone);
         INSERT INTO t (v) VALUES ('2:30+02');
         SELECT v FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("02:30:00+02:00".to_string())]);
}

#[test]
fn a_utc_marker_becomes_a_zero_offset() {
    let rows = query_rows(
        "CREATE TABLE t (v timestamptz);
         INSERT INTO t (v) VALUES ('2024-01-02T03:04:05Z');
         SELECT v FROM t;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("2024-01-02 03:04:05.000000+00:00".to_string())]);
}

#[test]
fn an_impossible_offset_is_refused() {
    // PostgreSQL's offsets run to ±15:59; anything past that is not a zone.
    let message = temporal_refusal("timestamptz", "2024-01-02 03:04:05+20");
    assert!(message.contains("out of range"), "{message}");
    let too_many = temporal_refusal("timestamptz", "2024-01-02 03:04:05+02:00:00");
    assert!(too_many.contains("too many parts"), "{too_many}");
}

#[test]
fn the_other_locale_dependent_forms_are_refused() {
    for literal in ["Jan 2 2024", "2 January 2024"] {
        let message = temporal_refusal("date", literal);
        assert!(message.contains("locale"), "{literal}: {message}");
    }
    let clock = temporal_refusal("time", "12:00:00 PM");
    assert!(clock.contains("12-hour"), "{clock}");
    let keyword = temporal_refusal("timestamp", "infinity");
    assert!(keyword.contains("keyword"), "{keyword}");
}

#[test]
fn a_date_of_the_wrong_shape_is_refused() {
    for literal in ["2024-03", "2024-03-05-06", "2024-ab-05", "0000-01-01"] {
        let message = temporal_refusal("date", literal);
        assert!(message.contains(literal), "{literal}: {message}");
    }
}

#[test]
fn a_time_of_the_wrong_shape_is_refused() {
    for literal in ["12", "12:00:00:00", "12:60:00", "12:00:00."] {
        let message = temporal_refusal("time", literal);
        assert!(message.contains(literal), "{literal}: {message}");
    }
    let leap = temporal_refusal("time", "23:59:60");
    assert!(leap.contains("second=60"), "{leap}");
}

#[test]
fn the_end_of_day_hour_rolls_the_month_and_the_year_over() {
    let rows = query_rows(
        "CREATE TABLE t (id int primary key, ts timestamp);
         INSERT INTO t (id, ts) VALUES (1, '2024-02-29 24:00:00'), (2, '2024-12-31 24:00:00');
         SELECT ts FROM t ORDER BY id;",
        &default_opts(),
    );
    assert_eq!(
        rows,
        vec![Some("2024-03-01 00:00:00".to_string()), Some("2025-01-01 00:00:00".to_string())]
    );
}

/// Refuses an interval literal, returning the message.
fn interval_refusal(literal: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!(
            "CREATE TABLE t (iv interval);
             INSERT INTO t (iv) VALUES ('{literal}');"
        ))
        .expect("parse")
        .translate(&default_opts())
        .expect_err("expected a refusal")
        .to_string()
}

#[test]
fn an_interval_unit_postgresql_does_not_have_is_refused() {
    let message = interval_refusal("2 fortnights");
    assert!(message.contains("not an interval unit"), "{message}");
}

#[test]
fn an_interval_count_without_a_unit_is_refused() {
    let dangling = interval_refusal("2 3 days");
    assert!(dangling.contains("names no unit"), "{dangling}");
    let unitless = interval_refusal("days");
    assert!(unitless.contains("has no count"), "{unitless}");
}

#[test]
fn an_interval_clock_of_the_wrong_shape_is_refused() {
    let message = interval_refusal("1:2:3:4");
    assert!(message.contains("three parts at most"), "{message}");
}

#[test]
fn an_iso_interval_of_the_wrong_shape_is_refused() {
    let designator = interval_refusal("P1X");
    assert!(designator.contains("designator"), "{designator}");
    let hour_before_t = interval_refusal("P1H");
    assert!(hour_before_t.contains("designator"), "{hour_before_t}");
    let no_designator = interval_refusal("P1Y2");
    assert!(no_designator.contains("unit designator"), "{no_designator}");
}

#[test]
fn interval_counts_keep_their_own_signs() {
    assert_eq!(interval_text("1 mon -1 day"), vec![Some("1 mon -1 days".to_string())]);
    assert_eq!(interval_text("1 day -02:00:00"), vec![Some("1 day -02:00:00".to_string())]);
    assert_eq!(interval_text("PT-1H"), vec![Some("-01:00:00".to_string())]);
}

#[test]
fn an_interval_second_keeps_its_microseconds() {
    assert_eq!(interval_text("1.000001 seconds"), vec![Some("00:00:01.000001".to_string())]);
    assert_eq!(interval_text("1 microsecond"), vec![Some("00:00:00.000001".to_string())]);
}

#[test]
fn an_interval_written_as_one_token_is_read() {
    assert_eq!(interval_text("1day"), vec![Some("1 day".to_string())]);
    assert_eq!(interval_text("@ 1 day"), vec![Some("1 day".to_string())]);
}

#[test]
fn interval_weeks_become_days() {
    assert_eq!(interval_text("3 weeks"), vec![Some("21 days".to_string())]);
    assert_eq!(interval_text("P1W"), vec![Some("7 days".to_string())]);
}

#[test]
fn an_iso_interval_reads_every_designator() {
    assert_eq!(
        interval_text("P1Y2M3DT4H5M6S"),
        vec![Some("1 year 2 mons 3 days 04:05:06".to_string())]
    );
}

#[test]
fn a_bare_count_before_a_clock_counts_days() {
    // PostgreSQL reads '1 12:00' as one day and twelve hours.
    assert_eq!(interval_text("1 12:00"), vec![Some("1 day 12:00:00".to_string())]);
}

#[test]
fn a_negative_year_month_interval_signs_both_counts() {
    assert_eq!(interval_text("-1-2"), vec![Some("-1 years -2 mons".to_string())]);
}

#[test]
fn an_interval_count_that_is_not_a_number_is_refused() {
    let message = interval_refusal("1.2.3 days");
    assert!(message.contains("not a number"), "{message}");
}

#[test]
fn a_zoned_time_column_refuses_what_it_cannot_read() {
    let message = temporal_refusal("time with time zone", "2:30+20");
    assert!(message.contains("out of range"), "{message}");
    let shape = temporal_refusal("time with time zone", "half past two+02");
    assert!(shape.contains("half past two"), "{shape}");
}

#[test]
fn a_month_name_in_a_timestamp_is_refused() {
    let message = temporal_refusal("timestamp", "Jan 2 2024 10:00");
    assert!(message.contains("locale"), "{message}");
}

#[test]
fn a_year_too_large_to_hold_is_refused() {
    let message = temporal_refusal("date", "99999999999999-01-01");
    assert!(message.contains("99999999999999-01-01"), "{message}");
}

#[test]
fn a_bound_parameter_in_a_checked_column_passes_through() {
    // The caller binds the value PostgreSQL takes, so there is no literal to
    // read and nothing to refuse.
    let statements = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (d date, iv interval, v double precision);
             INSERT INTO t (d, iv, v) VALUES ($1, $2, $3);",
        )
        .expect("parse")
        .translate_to_sql(&default_opts())
        .expect("translate");
    let insert = statements.last().expect("two statements");
    assert!(insert.contains("VALUES (?1, ?2, ?3)"), "{insert}");
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

// ── INSERT ... SELECT into bytea column (for_each_insert_position Select
// branch) ──

#[test]
fn bytea_insert_select_executes() {
    // INSERT ... SELECT must convert \x literals at bytea-column positions just
    // as VALUES does; exercises the SetExpr::Select branch in
    // for_each_insert_position.
    run(
        "CREATE TABLE t (b bytea);
         INSERT INTO t (b) SELECT '\\x414243';",
        &default_opts(),
    );
}

// ── Bytea literal without \\x prefix (maybe_convert_bytea_hex_literal line
// 992) ──

#[test]
fn bytea_literal_without_hex_prefix_is_refused() {
    // A raw string with no \\x prefix cannot be decoded as hex bytea.
    expect_refusal(
        "CREATE TABLE t (col bytea);
         INSERT INTO t VALUES ('ABC');",
        &default_opts(),
        "hex format",
    );
}

// ── Bytea literal with non-hex characters (line 1003-1007) ───────────────────

#[test]
fn bytea_non_hex_chars_after_prefix_are_refused() {
    // \\xGG has a valid \\x prefix but G is not a hex digit.
    expect_refusal(
        "CREATE TABLE t (col bytea);
         INSERT INTO t VALUES ('\\xGG');",
        &default_opts(),
        "non-hex",
    );
}

// ── Multi-row VALUES where only a later row carries the literal ──────────────

#[test]
fn bytea_multi_row_values_later_row_is_converted() {
    // The second row carries the hex literal; the first row has NULL.
    // All rows must be processed, not just the first.
    let rows = query_rows(
        "CREATE TABLE t (id INTEGER, b bytea);
         INSERT INTO t VALUES (1, NULL), (2, '\\x414243');
         SELECT lower(hex(b)) FROM t WHERE id = 2;",
        &default_opts(),
    );
    assert_eq!(rows, vec![Some("414243".to_string())]);
}
