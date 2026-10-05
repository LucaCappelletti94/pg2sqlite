//! A replica holds every `timestamptz` as UTC text with microseconds and an
//! explicit zero offset, `YYYY-MM-DD HH:MM:SS.ffffff+00:00`.
//!
//! The upload adapter refuses text without an offset, and a snapshot and a
//! live change of one row have to be byte-identical, so every write the
//! translation emits into a `timestamptz` column produces that one form.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use run_translated_helper::run_translated_with;

fn rows(pg: &str) -> Vec<Option<String>> {
    run_translated_with(pg, &Pg2SqliteOptions::default())
}

/// Whether `text` matches `shape`, where `d` stands for any digit.
fn has_shape(text: &str, shape: &str) -> bool {
    text.len() == shape.len()
        && text.bytes().zip(shape.bytes()).all(|(byte, expected)| {
            if expected == b'd' { byte.is_ascii_digit() } else { byte == expected }
        })
}

fn is_canonical(text: &str) -> bool {
    has_shape(text, "dddd-dd-dd dd:dd:dd.dddddd+00:00")
}

fn assert_all_canonical(rows: &[Option<String>]) {
    assert!(!rows.is_empty(), "the probe returned no rows");
    for row in rows {
        let text = row.as_deref().expect("the value should not be NULL");
        assert!(is_canonical(text), "{text} is not canonical timestamptz text");
    }
}

#[test]
fn every_now_spelling_as_a_column_default_writes_canonical_text() {
    for now in [
        "now()",
        "CURRENT_TIMESTAMP",
        "transaction_timestamp()",
        "statement_timestamp()",
        "clock_timestamp()",
    ] {
        let written = rows(&format!(
            "CREATE TABLE orders (id int PRIMARY KEY, created_at timestamptz NOT NULL DEFAULT {now});
             INSERT INTO orders (id) VALUES (1);
             INSERT INTO orders (id, created_at) VALUES (2, DEFAULT);
             SELECT created_at FROM orders;"
        ));
        assert_eq!(written.len(), 2, "{now}");
        assert_all_canonical(&written);
    }
}

#[test]
fn now_written_by_insert_update_and_upsert_is_canonical() {
    assert_all_canonical(&rows(
        "CREATE TABLE t (id int PRIMARY KEY, at timestamptz);
         INSERT INTO t (id, at) VALUES (1, now());
         SELECT at FROM t;",
    ));
    assert_all_canonical(&rows(
        "CREATE TABLE t (id int PRIMARY KEY, at timestamptz);
         INSERT INTO t (id) VALUES (1);
         UPDATE t SET at = now() WHERE id = 1;
         SELECT at FROM t;",
    ));
    assert_all_canonical(&rows(
        "CREATE TABLE t (id int PRIMARY KEY, at timestamptz);
         INSERT INTO t (id) VALUES (1);
         INSERT INTO t (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET at = now();
         SELECT at FROM t;",
    ));
}

#[test]
fn a_computed_timestamptz_is_written_canonical() {
    for (assignment, stored) in [
        ("at + interval '1 day'", "2026-09-24 13:42:07.000000+00:00"),
        ("date_trunc('day', at)", "2026-09-23 00:00:00.000000+00:00"),
        // A zoneless timestamp read in UTC keeps its fraction.
        ("naive AT TIME ZONE 'UTC'", "2026-09-23 13:42:07.500000+00:00"),
    ] {
        assert_eq!(
            rows(&format!(
                "CREATE TABLE t (id int PRIMARY KEY, at timestamptz, naive timestamp);
                 INSERT INTO t (id, at, naive)
                     VALUES (1, '2026-09-23 13:42:07+00', '2026-09-23 13:42:07.5');
                 UPDATE t SET at = {assignment} WHERE id = 1;
                 SELECT at FROM t;"
            )),
            vec![Some(stored.to_string())],
            "{assignment}"
        );
    }
}

#[test]
fn a_timestamptz_literal_is_written_in_utc_with_microseconds() {
    for (literal, stored) in [
        ("2026-09-23 13:42:07.957522+00", "2026-09-23 13:42:07.957522+00:00"),
        ("2026-09-23 15:42:07+02", "2026-09-23 13:42:07.000000+00:00"),
        ("2026-09-23 19:12:07.5+05:30", "2026-09-23 13:42:07.500000+00:00"),
        ("2026-09-23T13:42:07Z", "2026-09-23 13:42:07.000000+00:00"),
        ("2026-09-23 13:42:07", "2026-09-23 13:42:07.000000+00:00"),
        // PostgreSQL rounds the seventh digit with rint over the parsed
        // double, so 0.0000025 lands on 2 and 0.9999995 carries into the
        // second. Measured on PostgreSQL 16.
        ("2026-09-23 13:42:07.9999995+00", "2026-09-23 13:42:08.000000+00:00"),
        ("2026-09-23 13:42:07.0000025+00", "2026-09-23 13:42:07.000002+00:00"),
        ("2026-09-23 13:42:07.1234567+00", "2026-09-23 13:42:07.123457+00:00"),
        // Across midnight, the month and the year.
        ("2027-01-01 01:00:00+02", "2026-12-31 23:00:00.000000+00:00"),
        ("2024-02-28 22:00:00-03", "2024-02-29 01:00:00.000000+00:00"),
    ] {
        assert_eq!(
            rows(&format!(
                "CREATE TABLE t (at timestamptz);
                 INSERT INTO t (at) VALUES ('{literal}');
                 SELECT at FROM t;"
            )),
            vec![Some(stored.to_string())],
            "{literal}"
        );
    }
}

#[test]
fn a_timestamptz_literal_out_of_the_year_range_after_conversion_is_refused() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (at timestamptz);
             INSERT INTO t (at) VALUES ('0001-01-01 00:00:00+05');",
        )
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("a UTC instant before year 1 has no canonical text");
    assert!(error.to_string().contains("0001-01-01 00:00:00+05"), "{error}");
}

#[test]
fn a_timestamptz_column_copied_into_another_keeps_its_microseconds() {
    assert_eq!(
        rows(
            "CREATE TABLE a (at timestamptz);
             CREATE TABLE b (at timestamptz);
             INSERT INTO a (at) VALUES ('2026-09-23 13:42:07.957522+00');
             INSERT INTO b (at) SELECT at FROM a;
             SELECT at FROM b;",
        ),
        vec![Some("2026-09-23 13:42:07.957522+00:00".to_string())]
    );
}

#[test]
fn a_value_crossing_between_zoned_and_zoneless_columns_takes_the_target_form() {
    for (write, stored) in [
        ("INSERT INTO t (id, at) SELECT 2, naive FROM t", "2026-09-23 13:42:07.500000+00:00"),
        (
            "INSERT INTO t (id, at) SELECT 2, naive::timestamptz FROM t",
            "2026-09-23 13:42:07.500000+00:00",
        ),
        ("UPDATE t SET at = naive", "2026-09-23 13:42:07.500000+00:00"),
        (
            "INSERT INTO t (id, at) VALUES (1, now()) ON CONFLICT (id) DO UPDATE SET at = naive",
            "2026-09-23 13:42:07.500000+00:00",
        ),
        ("UPDATE t SET naive = at", "2026-09-23 13:42:07"),
        ("INSERT INTO t (id, naive) SELECT 2, at FROM t", "2026-09-23 13:42:07"),
    ] {
        let column = if stored.ends_with("+00:00") { "at" } else { "naive" };
        let written = rows(&format!(
            "CREATE TABLE t (id int PRIMARY KEY, at timestamptz, naive timestamp);
             INSERT INTO t (id, at, naive)
                 VALUES (1, '2026-09-23 13:42:07+00', '2026-09-23 13:42:07.5');
             {write};
             SELECT {column} FROM t ORDER BY id DESC LIMIT 1;"
        ));
        assert_eq!(written, vec![Some(stored.to_string())], "{write}");
    }
}

#[test]
fn a_trigger_assignment_writes_canonical_text() {
    assert_all_canonical(&rows(
        "CREATE TABLE brands (id int PRIMARY KEY, name text, edited_at timestamptz);
         CREATE FUNCTION touch() RETURNS trigger AS $$
         BEGIN
             NEW.edited_at = now();
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER brands_touch BEFORE INSERT OR UPDATE ON brands
             FOR EACH ROW EXECUTE FUNCTION touch();
         INSERT INTO brands (id, name) VALUES (1, 'a');
         SELECT edited_at FROM brands;",
    ));
}

#[test]
fn now_read_back_is_canonical_and_compares_with_stored_values() {
    assert_all_canonical(&rows("SELECT now();"));
    assert_all_canonical(&rows("SELECT CURRENT_TIMESTAMP;"));
    // A row written in this second is not in the future. Compared against
    // offset-less text it would be, since `...07.313000+00:00` sorts after
    // `...07`.
    assert_eq!(
        rows(
            "CREATE TABLE t (id int PRIMARY KEY, at timestamptz DEFAULT now());
             INSERT INTO t (id) VALUES (1);
             SELECT id FROM t WHERE at <= now();",
        ),
        vec![Some("1".to_string())]
    );
}

#[test]
fn now_written_into_another_temporal_column_takes_that_column_form() {
    for (declared, shape) in [
        ("timestamp", "dddd-dd-dd dd:dd:dd"),
        ("date", "dddd-dd-dd"),
        ("time", "dd:dd:dd"),
        ("time with time zone", "dd:dd:dd+00:00"),
    ] {
        let written = rows(&format!(
            "CREATE TABLE t (id int PRIMARY KEY, at {declared} DEFAULT now());
             INSERT INTO t (id) VALUES (1);
             INSERT INTO t (id, at) VALUES (2, now()), (3, (now()));
             SELECT at FROM t;"
        ));
        assert_eq!(written.len(), 3, "{declared}");
        for row in written {
            let text = row.expect("the value should not be NULL");
            assert!(has_shape(&text, shape), "{declared}: {text} is not {shape}");
        }
    }
}

#[test]
fn a_column_without_a_zone_compares_with_now_as_an_instant() {
    // PostgreSQL widens today's date to its midnight, which is not now.
    for (predicate, count) in [
        ("d = now()", "0"),
        ("d IN (now())", "0"),
        ("d IS NOT DISTINCT FROM now()", "0"),
        ("d BETWEEN '2000-01-01' AND now()", "1"),
    ] {
        assert_eq!(
            rows(&format!(
                "CREATE TABLE t (id int PRIMARY KEY, d date DEFAULT now());
                 INSERT INTO t (id) VALUES (1);
                 SELECT count(*) FROM t WHERE {predicate};"
            )),
            vec![Some(count.to_string())],
            "{predicate}"
        );
    }
}

#[test]
fn a_default_forwarded_by_a_row_level_security_trigger_keeps_the_column_form() {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    for (declared, canonical) in [("timestamptz", true), ("timestamp", false)] {
        let written = run_translated_with(
            &format!(
                "CREATE TABLE t (id int PRIMARY KEY, at {declared} DEFAULT now());
                 ALTER TABLE t ENABLE ROW LEVEL SECURITY;
                 CREATE POLICY p ON t USING (true) WITH CHECK (true);
                 INSERT INTO t (id) VALUES (1);
                 SELECT at FROM t;"
            ),
            &options,
        );
        let text = written[0].as_deref().expect("the value should not be NULL");
        assert_eq!(is_canonical(text), canonical, "{declared}: {text}");
        assert_eq!(text.contains('+'), canonical, "{declared}: {text}");
    }
}

#[test]
fn the_canonical_form_reverses_to_now_or_a_timestamptz_cast() {
    let translator = Pg2Sqlite::default().sql("CREATE TABLE t (at timestamptz);").expect("parse");
    let schema = translator.build_schema().expect("schema");
    for (sqlite, postgres) in [
        ("SELECT strftime('%Y-%m-%d %H:%M:%f000+00:00', 'now')", "SELECT NOW()"),
        (
            "SELECT strftime('%Y-%m-%d %H:%M:%f000+00:00', at) FROM t",
            "SELECT at::TIMESTAMPTZ FROM t",
        ),
    ] {
        let reversed =
            translator.reverse_sql(sqlite, &schema, &Pg2SqliteOptions::default()).expect("reverse");
        assert_eq!(
            reversed.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [postgres],
            "{sqlite}"
        );
    }
    // A trailing modifier shifts the instant, so it cannot collapse to NOW().
    translator
        .reverse_sql(
            "SELECT strftime('%Y-%m-%d %H:%M:%f000+00:00', 'now', '+1 day')",
            &schema,
            &Pg2SqliteOptions::default(),
        )
        .expect_err("the modifier has no PostgreSQL form here");
}
