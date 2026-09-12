//! `floor`, `ceil`, and `trunc` over a `NUMERIC(p,s)` column.
//!
//! A `NUMERIC(10,2)` column stores `1.50` as `150` (minor units). Before this
//! fix, the three functions ran on the raw stored integer, so `floor(1.50)`
//! answered `150` where PostgreSQL answers `1`.
//!
//! All expected values were read from PostgreSQL 17 before the fix was written.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::Pg2SqliteOptions;
use run_translated_helper::run_translated_with;

const FIXTURE: &str = "
    CREATE TABLE t (id INT PRIMARY KEY, amount NUMERIC(10,2));
    INSERT INTO t VALUES
        (1,  1.50),
        (2, -1.50),
        (3,  0.25),
        (4,  1.55),
        (5, -0.25),
        (6,  2.00);
";

fn run(script: &str) -> Vec<Option<String>> {
    run_translated_with(script, &Pg2SqliteOptions::default())
}

// ── floor ────────────────────────────────────────────────────────────────────

/// `floor(1.50) = 1`, `floor(-1.50) = -2`, `floor(0.25) = 0`.
/// All measured on PostgreSQL 17.
#[test]
fn floor_of_numeric_column_answers_like_postgres() {
    let rows = run(&format!(
        "{FIXTURE}
         SELECT floor(amount) FROM t WHERE id IN (1, 2, 3) ORDER BY id;"
    ));
    assert_eq!(
        rows,
        vec![Some("1".to_string()), Some("-2".to_string()), Some("0".to_string())],
        "floor(1.50)=1, floor(-1.50)=-2, floor(0.25)=0"
    );
}

/// `floor(amount) + 1` must add as plain integers.
/// PostgreSQL: `floor(1.50) + 1 = 2`.
#[test]
fn floor_amount_plus_one_composes_correctly() {
    let rows = run(&format!(
        "{FIXTURE}
         SELECT floor(amount) + 1 FROM t WHERE id = 1;"
    ));
    assert_eq!(rows, vec![Some("2".to_string())], "floor(1.50) + 1 = 2");
}

/// `WHERE floor(amount) = 1` must match rows whose floor value is 1.
#[test]
fn where_floor_amount_equals_integer_filters_correctly() {
    let rows = run(&format!(
        "{FIXTURE}
         SELECT id FROM t WHERE floor(amount) = 1;"
    ));
    // floor(1.50) = 1 ✓; floor(2.00) = 2 ≠ 1; floor(-1.50) = -2 ≠ 1
    assert_eq!(
        rows,
        vec![Some("1".to_string()), Some("4".to_string())],
        "floor(1.50)=1 and floor(1.55)=1"
    );
}

/// `floor` over a REAL column uses CASE WHEN — no math functions needed.
#[test]
fn floor_of_real_column_works_without_math_functions() {
    let rows = run("CREATE TABLE r (id INT PRIMARY KEY, x REAL);
         INSERT INTO r VALUES (1, 1.7), (2, -1.7);
         SELECT floor(x) FROM r ORDER BY id;");
    assert_eq!(
        rows,
        vec![Some("1".to_string()), Some("-2".to_string())],
        "floor(REAL) via CASE WHEN gives INTEGER"
    );
}

// ── ceil ─────────────────────────────────────────────────────────────────────

/// `ceil(1.50) = 2`, `ceil(-1.50) = -1`, `ceil(0.25) = 1`.
/// All measured on PostgreSQL 17.
#[test]
fn ceil_of_numeric_column_answers_like_postgres() {
    let rows = run(&format!(
        "{FIXTURE}
         SELECT ceil(amount) FROM t WHERE id IN (1, 2, 3) ORDER BY id;"
    ));
    assert_eq!(
        rows,
        vec![Some("2".to_string()), Some("-1".to_string()), Some("1".to_string())],
        "ceil(1.50)=2, ceil(-1.50)=-1, ceil(0.25)=1"
    );
}

/// `ceiling` is the SQL-standard spelling and must behave identically to
/// `ceil`.
#[test]
fn ceiling_is_the_same_as_ceil() {
    let rows = run(&format!(
        "{FIXTURE}
         SELECT ceiling(amount) FROM t WHERE id IN (1, 2) ORDER BY id;"
    ));
    assert_eq!(
        rows,
        vec![Some("2".to_string()), Some("-1".to_string())],
        "ceiling(1.50)=2, ceiling(-1.50)=-1"
    );
}

/// `ceil` over a REAL column uses CASE WHEN — no math functions needed.
#[test]
fn ceil_of_real_column_works_without_math_functions() {
    let rows = run("CREATE TABLE r (id INT PRIMARY KEY, x REAL);
         INSERT INTO r VALUES (1, 1.7), (2, -1.2);
         SELECT ceil(x) FROM r ORDER BY id;");
    assert_eq!(
        rows,
        vec![Some("2".to_string()), Some("-1".to_string())],
        "ceil(REAL) via CASE WHEN gives INTEGER"
    );
}

// ── trunc (single-argument) ──────────────────────────────────────────────────

/// `trunc(1.50) = 1`, `trunc(-1.50) = -1`, `trunc(0.25) = 0`.
/// All measured on PostgreSQL 17.
#[test]
fn trunc_one_arg_of_numeric_column_answers_like_postgres() {
    let rows = run(&format!(
        "{FIXTURE}
         SELECT trunc(amount) FROM t WHERE id IN (1, 2, 3) ORDER BY id;"
    ));
    assert_eq!(
        rows,
        vec![Some("1".to_string()), Some("-1".to_string()), Some("0".to_string())],
        "trunc(1.50)=1, trunc(-1.50)=-1, trunc(0.25)=0"
    );
}

/// `trunc(amount) + 1` composes as plain integers.
/// PostgreSQL: `trunc(1.50) + 1 = 2`.
#[test]
fn trunc_amount_plus_one_composes_correctly() {
    let rows = run(&format!(
        "{FIXTURE}
         SELECT trunc(amount) + 1 FROM t WHERE id = 1;"
    ));
    assert_eq!(rows, vec![Some("2".to_string())], "trunc(1.50) + 1 = 2");
}

/// `WHERE trunc(amount) = 1` matches the row holding `1.50` and `2.00`
/// but not `1.55` (whose trunc is still 1, in fact it also matches).
#[test]
fn where_trunc_amount_equals_integer_filters_correctly() {
    let rows = run(&format!(
        "{FIXTURE}
         SELECT id FROM t WHERE trunc(amount) = 1 ORDER BY id;"
    ));
    // trunc(1.50) = 1, trunc(1.55) = 1 both match; 2.00 → 2 does not
    assert_eq!(
        rows,
        vec![Some("1".to_string()), Some("4".to_string())],
        "trunc matches rows whose integer part is 1"
    );
}

/// The single-argument form still works for REAL operands (unchanged).
#[test]
fn trunc_one_arg_of_real_is_unchanged() {
    let rows = run("CREATE TABLE r (id INT PRIMARY KEY, x REAL);
         INSERT INTO r VALUES (1, 3.7), (2, -3.7);
         SELECT trunc(x) FROM r ORDER BY id;");
    assert_eq!(
        rows,
        vec![Some("3".to_string()), Some("-3".to_string())],
        "trunc(x) still casts REAL to INTEGER for non-NUMERIC columns"
    );
}

// ── trunc (two-argument) ─────────────────────────────────────────────────────

/// `trunc(1.55, 1) = 1.5`, `trunc(-1.55, 1) = -1.5`.
/// Measured on PostgreSQL 17. The old emission answered `155` as REAL for
/// stored `1.55`, which is off by a factor of 100.
#[test]
fn trunc_two_arg_of_numeric_column_answers_like_postgres() {
    let rows = run(&format!(
        "{FIXTURE}
         SELECT trunc(amount, 1) FROM t WHERE id IN (4, 2) ORDER BY id;"
    ));
    // id=2 is -1.50, trunc(-1.50, 1) = -1.5
    // id=4 is  1.55, trunc( 1.55, 1) =  1.5
    assert_eq!(
        rows,
        vec![Some("-1.5".to_string()), Some("1.5".to_string())],
        "trunc(x, 1): id=2 → -1.5, id=4 → 1.5"
    );
}

/// `trunc(amount, 0)` truncates to the integer — same as the one-argument form.
#[test]
fn trunc_two_arg_zero_places_equals_one_arg() {
    let rows = run(&format!(
        "{FIXTURE}
         SELECT trunc(amount, 0) FROM t WHERE id IN (1, 2) ORDER BY id;"
    ));
    assert_eq!(
        rows,
        vec![Some("1".to_string()), Some("-1".to_string())],
        "trunc(x, 0) == trunc(x) for NUMERIC"
    );
}

/// `trunc(amount, -1)` truncates to the tens place.
/// `trunc(1.55, -1)` = `0` in PostgreSQL.
#[test]
fn trunc_two_arg_negative_scale_of_numeric() {
    let rows = run(&format!(
        "{FIXTURE}
         SELECT trunc(amount, -1) FROM t WHERE id = 4;"
    ));
    // trunc(1.55, -1) = 0 in PostgreSQL
    assert_eq!(rows, vec![Some("0".to_string())], "trunc(1.55, -1) = 0");
}

/// `trunc(x, n)` for `n >= s` (the column scale) is a no-op: all s decimal
/// places are already within the precision asked for. `trunc(1.55, 2) = 1.55`.
#[test]
fn trunc_two_arg_scale_at_or_above_column_scale_is_identity() {
    let rows = run(&format!(
        "{FIXTURE}
         SELECT trunc(amount, 2) FROM t WHERE id = 4;"
    ));
    // trunc(1.55, 2) = 1.55 — all decimal places kept
    assert_eq!(rows, vec![Some("1.55".to_string())], "trunc(1.55, 2) = 1.55");
}

/// The two-argument form still works for REAL operands (existing behaviour
/// must not regress).
#[test]
fn trunc_two_arg_of_real_is_unchanged() {
    let rows = run("CREATE TABLE r (id INT PRIMARY KEY, x REAL);
         INSERT INTO r VALUES (1, 1.789), (2, -1.789);
         SELECT trunc(x, 2) FROM r ORDER BY id;");
    assert_eq!(
        rows,
        vec![Some("1.78".to_string()), Some("-1.78".to_string())],
        "trunc(x, 2) on REAL still uses the existing REAL-based truncation"
    );
}
