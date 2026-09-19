//! Scale inference over expression shapes: B1–B6, B8.
//!
//! Fixture: `amount NUMERIC(10,2)`, six rows (1.00, 1.00, 1.00, 2.00, 2.50,
//! 1.50) unless a test declares its own.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

/// Setup SQL used by most tests in this file.
const SETUP: &str = "
CREATE TABLE t (
    id   INT PRIMARY KEY,
    tag  TEXT NOT NULL,
    amount NUMERIC(10,2) NOT NULL
);
INSERT INTO t VALUES
    (1, 'b', 1.00),
    (2, 'b', 1.00),
    (3, 'b', 1.00),
    (4, 'a', 2.00),
    (5, 'b', 2.50),
    (6, 'a', 1.50);
";

fn run(pg: &str) -> Vec<Option<String>> {
    run_translated_with(pg, &Pg2SqliteOptions::default())
}

fn refuse(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("should refuse")
        .to_string()
}

// ── B1: scale-preserving calls abs, round, avg ──────────────────────────────

/// `abs(amount) = 1.5` must answer the one row where abs(1.50) = 1.50.
#[test]
fn abs_of_numeric_column_scales_literal() {
    let rows = run(&format!("{SETUP} SELECT count(*) FROM t WHERE abs(amount) = 1.5;"));
    assert_eq!(rows, vec![Some("1".to_string())]);
}

/// `amount + 1.5 = 3` — left is a binary-op at scale 2, right must scale.
#[test]
fn arithmetic_result_scales_the_comparison_literal() {
    let rows = run(&format!("{SETUP} SELECT count(*) FROM t WHERE amount + 1.5 = 3;"));
    assert_eq!(rows, vec![Some("1".to_string())]);
}

/// `round(amount, 0) = 2` — round is scale-preserving; literal scales.
#[test]
fn round_of_numeric_scales_literal() {
    let rows = run(&format!("{SETUP} SELECT count(*) FROM t WHERE round(amount, 0) = 2;"));
    // round(1.00)=1, round(1.00)=1, round(1.00)=1, round(2.00)=2,
    // round(2.50)=3 (half-away-from-zero), round(1.50)=2 → 2 rows
    assert_eq!(rows, vec![Some("2".to_string())]);
}

/// `avg(amount) = 1.5` — avg is treated as scale-preserving.
#[test]
fn avg_of_numeric_scales_literal() {
    let rows = run(&format!("{SETUP} SELECT (avg(amount) = 1.5) FROM t;"));
    // avg(1.00+1.00+1.00+2.00+2.50+1.50)/6 = 9.00/6 = 1.50
    assert_eq!(rows, vec![Some("1".to_string())]);
}

/// `(SELECT max(amount) FROM t) = 2.5` — scalar subquery at scale 2.
#[test]
fn scalar_subquery_scales_comparison_literal() {
    let rows =
        run(&format!("{SETUP} SELECT count(*) FROM t WHERE (SELECT max(amount) FROM t) = 2.5;"));
    // max = 2.50, so condition is true for all 6 rows
    assert_eq!(rows, vec![Some("6".to_string())]);
}

// ── B2: ANY(ARRAY[...]) literals scale ──────────────────────────────────────

/// `amount = ANY(ARRAY[1.5, 2.5])` synthesises a scaled InList.
#[test]
fn any_array_literals_are_scaled() {
    let rows =
        run(&format!("{SETUP} SELECT count(*) FROM t WHERE amount = ANY (ARRAY[1.5, 2.5]);"));
    // 1.50 and 2.50 match → 2 rows
    assert_eq!(rows, vec![Some("2".to_string())]);
}

// ── B3: CASE result arms scale ───────────────────────────────────────────────

/// `SELECT CASE WHEN tag = 'a' THEN 1.5 ELSE amount END` must return uniform
/// minor units; rows with tag 'a' become 150 and others keep their value.
#[test]
fn case_literal_arm_scales_to_column_arm_scale() {
    let rows = run(&format!(
        "{SETUP} SELECT CASE WHEN tag = 'a' THEN 1.5 ELSE amount END FROM t ORDER BY id;"
    ));
    // id 1,2,3,5 → amount (100,100,100,250); id 4,6 → 1.50 = 150 minor units
    assert_eq!(
        rows,
        vec![
            Some("100".to_string()),
            Some("100".to_string()),
            Some("100".to_string()),
            Some("150".to_string()),
            Some("250".to_string()),
            Some("150".to_string()),
        ]
    );
}

/// `UPDATE t SET amount = CASE WHEN tag='a' THEN 1.5 ELSE 2.0 END` must store
/// minor-unit integers so STRICT accepts them.
#[test]
fn case_all_literal_arms_scale_in_update() {
    let rows = run(&format!(
        "{SETUP}
         UPDATE t SET amount = CASE WHEN tag = 'a' THEN 1.5 ELSE 2.0 END;
         SELECT amount FROM t ORDER BY id;"
    ));
    // tag 'a' rows → 150, tag 'b' rows → 200
    assert_eq!(
        rows,
        vec![
            Some("200".to_string()),
            Some("200".to_string()),
            Some("200".to_string()),
            Some("150".to_string()),
            Some("200".to_string()),
            Some("150".to_string()),
        ]
    );
}

// ── B4: cross-scale comparison rescales the narrower side ───────────────────

/// `amount = micros` where amount is scale 2 and micros is scale 4.
#[test]
fn cross_scale_comparison_rescales_narrower_side() {
    let rows =
        run("CREATE TABLE t (id INT PRIMARY KEY, amount NUMERIC(10,2), micros NUMERIC(10,4));
         INSERT INTO t VALUES
             (1, 1.00, 1.0000),
             (2, 1.00, 1.0000),
             (3, 1.00, 1.0000),
             (4, 2.00, 2.0000),
             (5, 2.50, 2.5000),
             (6, 1.50, 2.0000);
         SELECT count(*) FROM t WHERE amount = micros;");
    // rows 1–5 match, row 6 (1.50 ≠ 2.00) does not → 5 rows
    assert_eq!(rows, vec![Some("5".to_string())]);
}

// ── B5: IS DISTINCT FROM scales NUMERIC literals ────────────────────────────

/// `amount IS DISTINCT FROM 1.5` must answer only the 5 rows where amount ≠
/// 1.50.
#[test]
fn is_distinct_from_scales_numeric_literal() {
    let rows = run(&format!("{SETUP} SELECT count(*) FROM t WHERE amount IS DISTINCT FROM 1.5;"));
    assert_eq!(rows, vec![Some("5".to_string())]);
}

/// `amount IS NOT DISTINCT FROM 1.5` must answer the 1 row where amount = 1.50.
#[test]
fn is_not_distinct_from_scales_numeric_literal() {
    let rows =
        run(&format!("{SETUP} SELECT count(*) FROM t WHERE amount IS NOT DISTINCT FROM 1.5;"));
    assert_eq!(rows, vec![Some("1".to_string())]);
}

// ── B6: division of a NUMERIC value is refused ──────────────────────────────

/// `sum(amount) / count(*)` must be refused: the gate was extended to cover
/// one-sided NUMERIC operands.
#[test]
fn numeric_divided_by_aggregate_is_refused() {
    let error = refuse(&format!("{SETUP} SELECT sum(amount) / count(*) FROM t;"));
    assert!(error.to_lowercase().contains("divid"), "refusal must name division, got: {error}");
}

/// `amount / 2` is also refused: the column holds minor units.
#[test]
fn numeric_column_divided_by_literal_is_refused() {
    let error = refuse(&format!("{SETUP} SELECT amount / 2 FROM t;"));
    assert!(error.to_lowercase().contains("divid"), "refusal must name division, got: {error}");
}

// ── B8: NUMERIC::text renders the decimal form ──────────────────────────────

/// `amount::text` must render `'1.50'` rather than `'150'`.
#[test]
fn numeric_cast_to_text_renders_decimal() {
    let rows = run("CREATE TABLE t (id INT PRIMARY KEY, amount NUMERIC(10,2));
         INSERT INTO t VALUES (1, 1.50), (2, -1.50);
         SELECT amount::text FROM t ORDER BY id;");
    assert_eq!(rows, vec![Some("1.50".to_string()), Some("-1.50".to_string())]);
}

/// `WHERE amount::text = '1.50'` must match the one row.
#[test]
fn numeric_cast_to_text_filters_correctly() {
    let rows = run("CREATE TABLE t (id INT PRIMARY KEY, amount NUMERIC(10,2));
         INSERT INTO t VALUES (1, 1.50), (2, 2.00);
         SELECT count(*) FROM t WHERE amount::text = '1.50';");
    assert_eq!(rows, vec![Some("1".to_string())]);
}

/// A scaled column compared against an unscaled numeric one: PostgreSQL
/// promotes the integer, so `1.50 > 2` is false and `1.50 = 150` is false.
/// The stored minor units make both true unless the other side is brought
/// onto the same scale.
#[test]
fn a_scaled_column_compared_against_an_integer_column() {
    let setup = "CREATE TABLE t (id INT PRIMARY KEY, amount NUMERIC(10,2), n INT);
         INSERT INTO t VALUES (1, 1.50, 2), (2, 1.50, 150);";
    assert_eq!(
        run(&format!("{setup}\nSELECT count(*) FROM t WHERE amount > n;")),
        vec![Some("0".to_string())],
        "1.50 is greater than neither 2 nor 150"
    );
    assert_eq!(
        run(&format!("{setup}\nSELECT count(*) FROM t WHERE amount = n;")),
        vec![Some("0".to_string())],
        "1.50 equals neither 2 nor 150"
    );
    assert_eq!(
        run(&format!("{setup}\nSELECT count(*) FROM t WHERE amount IS DISTINCT FROM n;")),
        vec![Some("2".to_string())],
        "both rows differ"
    );
}

/// A qualified reference keeps its column's scale even where a PL/pgSQL
/// variable carries the same name.
///
/// The scope declines a bare name a variable holds, since a variable is
/// neither resolved nor refused, and it used to decline a qualified one by
/// that same bare name. `t.amount` is the column, so the comparison literal
/// still scales into minor units, and emitting `1.50` against a column
/// holding `150` matches nothing.
#[test]
fn a_qualified_reference_scales_under_a_variable_of_the_same_name() {
    let body = |variable: &str| {
        format!(
            "CREATE TABLE t (id INT PRIMARY KEY, amount NUMERIC(10,2));\n\
             CREATE TABLE log (id INT PRIMARY KEY);\n\
             CREATE FUNCTION mark() RETURNS TRIGGER LANGUAGE plpgsql AS $$\n\
             DECLARE {variable} NUMERIC(10,2) := 1.50;\n\
             BEGIN\n\
               IF EXISTS (SELECT 1 FROM t WHERE t.amount = 1.50) THEN\n\
                 RAISE EXCEPTION 'hit';\n\
               END IF;\n\
               RETURN NEW;\n\
             END;\n\
             $$;\n\
             CREATE TRIGGER mark_log BEFORE INSERT ON log FOR EACH ROW EXECUTE FUNCTION mark();"
        )
    };
    for variable in ["amount", "threshold"] {
        let statements = Pg2Sqlite::default()
            .sql(&body(variable))
            .expect("parse")
            .translate_to_sql(&Pg2SqliteOptions::default())
            .expect("translate");
        let trigger = statements
            .iter()
            .find(|statement| statement.contains("t.amount ="))
            .expect("the trigger carries the comparison");
        assert!(
            trigger.contains("t.amount = 150"),
            "a variable named {variable} must not hide the column's scale: {trigger}"
        );
    }
}

/// A bare reference to a declared variable is the variable, so a column of
/// the same name does not lend it a scale.
///
/// The variable holds `1.50` and the column holds minor units, so scaling the
/// comparison literal here would compare the substituted `1.50` against
/// `150`.
#[test]
fn a_bare_variable_is_not_scaled_by_a_column_of_the_same_name() {
    let statements = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, amount NUMERIC(10,2));\n\
             CREATE TABLE log (id INT PRIMARY KEY);\n\
             CREATE FUNCTION mark() RETURNS TRIGGER LANGUAGE plpgsql AS $$\n\
             DECLARE amount NUMERIC(10,2) := 1.50;\n\
             BEGIN\n\
               IF amount = 1.50 THEN\n\
                 RAISE EXCEPTION 'hit';\n\
               END IF;\n\
               RETURN NEW;\n\
             END;\n\
             $$;\n\
             CREATE TRIGGER mark_log BEFORE INSERT ON log FOR EACH ROW EXECUTE FUNCTION mark();",
        )
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate");
    let trigger = statements
        .iter()
        .find(|statement| statement.contains("RAISE(ABORT, 'hit')"))
        .expect("the trigger carries the condition");
    assert!(
        trigger.contains("(SELECT 1.50) = 1.50"),
        "the variable keeps its own value on both sides: {trigger}"
    );
}
