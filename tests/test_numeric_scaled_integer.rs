//! `NUMERIC(p,s)` as an INTEGER scaled by `10^s`, per decision D1.
//!
//! Measured on PostgreSQL 16 over `NUMERIC(10,2)` columns holding 0.10 and
//! 0.20: `sum(price)` is 0.30 and `0.1 + 0.2 = 0.3` is TRUE. Measured on SQLite
//! 3.51.1 with the same columns mapped to REAL: the sum is
//! 0.30000000000000004 and the comparison is FALSE.
//!
//! Minor units make both exact, at the cost of a visible representation change:
//! the column now holds 10 and 20, and 0.30 reads as 30. R46g publishes the
//! scale through the manifest so a consumer can adapt.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

fn translate(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .join("\n")
}

fn refuse(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("this declaration cannot be represented as a scaled integer")
        .to_string()
}

#[test]
fn a_scaled_numeric_becomes_an_integer() {
    let sql = translate("CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(10,2));");
    assert!(sql.contains("price INTEGER"), "expected a scaled integer column: {sql}");
    assert!(!sql.to_uppercase().contains("REAL"), "REAL is what loses the exactness: {sql}");
}

/// SQLite promotes an overflowing integer to REAL with no error, so the bound
/// is the only thing that turns overflow into a failure. Measured:
/// `SELECT 9223372036854775807 + 1` answers 9.223372036854776e+18.
#[test]
fn the_precision_bound_is_emitted_and_enforced() {
    let sql = translate("CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(4,2));");
    assert!(sql.contains("9999"), "expected the 10^p - 1 bound as a literal: {sql}");

    let error = run_translated_or_error(
        "CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(4,2));
         INSERT INTO t VALUES (1, 1000);
         SELECT price FROM t;",
    )
    .expect_err("a value past the declared precision must fail");
    assert!(error.contains("CHECK"), "expected the bound to reject it, got: {error}");
}

/// `NUMERIC(p)` is scale 0 in PostgreSQL, so it is already a plain integer.
#[test]
fn an_unscaled_numeric_is_a_plain_integer() {
    let sql = translate("CREATE TABLE t (id INT PRIMARY KEY, n NUMERIC(10));");
    assert!(sql.contains("n INTEGER"), "{sql}");
}

/// i64 holds 10^18 - 1 but not 10^19 - 1, so 19 digits cannot be a scaled
/// integer and silently degrading to REAL is what this item removes.
#[test]
fn a_precision_past_eighteen_is_refused() {
    let error = refuse("CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(19,2));");
    assert!(error.contains("19"), "the error must name the precision, got: {error}");
    assert!(
        translate("CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(18,2));")
            .contains("price INTEGER"),
        "18 is the last one that fits"
    );
}

/// Bare NUMERIC has arbitrary unconstrained scale, so there is no `10^s` to
/// scale by and choosing one would corrupt data.
#[test]
fn a_bare_numeric_or_decimal_is_refused() {
    for declaration in ["NUMERIC", "DECIMAL"] {
        let error = refuse(&format!("CREATE TABLE t (id INT PRIMARY KEY, price {declaration});"));
        assert!(
            error.to_lowercase().contains("scale"),
            "the error must say what is missing, got: {error}"
        );
    }
}

/// The requirement D1 exists to serve, stated as PostgreSQL states it.
#[test]
fn exact_decimal_arithmetic_holds() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, a NUMERIC(10,2), b NUMERIC(10,2));
         INSERT INTO t VALUES (1, 0.10, 0.20);
         SELECT sum(a) + sum(b) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("30".to_string())], "minor units, so 0.30 reads as 30");
}

fn run_translated_or_error(pg: &str) -> Result<Vec<Option<String>>, String> {
    std::panic::catch_unwind(|| run_translated_with(pg, &Pg2SqliteOptions::default())).map_err(
        |payload| {
            payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(ToString::to_string))
                .unwrap_or_default()
        },
    )
}

/// A cast moves the point. Measured on PostgreSQL 16: `1.005::numeric(10,2)` is
/// 1.01 and `(-1.005)::numeric(10,2)` is -1.01, so the rounding is away from
/// zero on both signs, where SQLite's integer division truncates toward it.
#[test]
fn a_cast_rescales_and_rounds_away_from_zero() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, fine NUMERIC(10,3));
         INSERT INTO t VALUES (1, 1.005), (2, -1.005), (3, 2.675);
         SELECT CAST(fine AS NUMERIC(10,2)) FROM t ORDER BY id;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(
        rows,
        vec![Some("101".to_string()), Some("-101".to_string()), Some("268".to_string())],
        "1.005 to 1.01, -1.005 to -1.01, 2.675 to 2.68, all in minor units"
    );
}

/// An integer is a whole number, so casting it up is an exact multiply.
#[test]
fn a_cast_from_an_integer_multiplies_up() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT);
         INSERT INTO t VALUES (1, 7);
         SELECT CAST(n AS NUMERIC(10,2)) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("700".to_string())], "7 becomes 7.00, which is 700 minor units");
}

/// A decimal literal written against a scaled column has to be scaled too, at
/// translation time, or the INSERT stores a float into an INTEGER column and
/// STRICT rejects it.
#[test]
fn a_decimal_literal_is_scaled_on_insert() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(10,2));
         INSERT INTO t VALUES (1, 19.99), (2, 5), (3, -0.05);
         SELECT price FROM t ORDER BY id;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(
        rows,
        vec![Some("1999".to_string()), Some("500".to_string()), Some("-5".to_string())]
    );
}

/// The same scaling at a comparison site, which is where a missed literal
/// silently returns no rows rather than failing.
#[test]
fn a_decimal_literal_is_scaled_in_a_comparison() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(10,2));
         INSERT INTO t VALUES (1, 19.99);
         SELECT count(*) FROM t WHERE price = 19.99 AND price > 19.98 AND price < 20;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
}

/// A literal carrying more decimals than the column can hold is a translation
/// error rather than a silent round, since PostgreSQL would round and the
/// author probably meant a different scale.
#[test]
fn a_literal_finer_than_the_column_is_refused() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(10,2));
              INSERT INTO t VALUES (1, 19.999);",
        )
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("19.999 does not fit a scale of 2")
        .to_string();
    assert!(error.contains("19.999"), "the error must name the literal, got: {error}");
}

/// Addition needs both sides at one scale. Measured on PostgreSQL 16 with
/// `a NUMERIC(10,2) = 1.50` and `b NUMERIC(10,4) = 2.2500`: `a + b` is 3.7500
/// and `a - b` is -0.7500, both at the greater of the two scales.
#[test]
fn addition_brings_both_sides_to_one_scale() {
    const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, a NUMERIC(10,2), b NUMERIC(10,4));
         INSERT INTO t VALUES (1, 1.50, 2.2500);";
    let sum =
        run_translated_with(&format!("{TABLE} SELECT a + b FROM t;"), &Pg2SqliteOptions::default());
    assert_eq!(sum, vec![Some("37500".to_string())], "3.7500 at scale 4");
    let difference =
        run_translated_with(&format!("{TABLE} SELECT a - b FROM t;"), &Pg2SqliteOptions::default());
    assert_eq!(difference, vec![Some("-7500".to_string())], "-0.7500 at scale 4");
}

/// Multiplication lands at the sum of the scales with no rescaling, which is
/// what makes minor units elegant here: PostgreSQL answers 3.375000 for
/// `1.50 * 2.2500`, scale 6, and the integer product is already that.
#[test]
fn multiplication_lands_at_the_sum_of_the_scales() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, a NUMERIC(5,2), b NUMERIC(5,4));
         INSERT INTO t VALUES (1, 1.50, 2.2500);
         SELECT a * b FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("3375000".to_string())], "3.375000 at scale 6");
}

/// A product needing more than 18 digits cannot be held, and rescaling it
/// silently would change the value, so it is refused with both operand types
/// named.
#[test]
fn a_product_past_the_precision_ceiling_is_refused() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, a NUMERIC(10,2), b NUMERIC(10,2));
              SELECT a * b FROM t;",
        )
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("20 digits do not fit")
        .to_string();
    assert!(error.contains("NUMERIC(10,2)"), "the error must name the operands, got: {error}");
}

/// PostgreSQL picks a division result scale from both operand precisions,
/// measured as `NUMERIC(10,2) / integer` answering at scale 20, and SQLite's
/// integer division truncates toward zero on top of that. Any scale chosen here
/// would disagree by a different amount for every pair of operands, so the
/// operation is refused rather than approximated.
#[test]
fn dividing_two_numerics_is_refused() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, a NUMERIC(10,2), b NUMERIC(10,2));
              SELECT a / b FROM t;",
        )
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("division has no faithful form")
        .to_string();
    assert!(error.contains("scale"), "the error must explain why, got: {error}");
}

/// `sum`, `min`, and `max` keep the operand's scale, so they need nothing
/// beyond the representation itself.
#[test]
fn scale_preserving_aggregates_stay_exact() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(10,2));
         INSERT INTO t VALUES (1, 0.10), (2, 0.20), (3, 19.99);
         SELECT sum(price) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("2029".to_string())], "20.29 at scale 2, exactly");
}

/// Ordering is native integer ordering, which is the same order as the decimal
/// values, unlike a TEXT representation.
#[test]
fn ordering_is_numeric() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(10,2));
         INSERT INTO t VALUES (1, 9.90), (2, 10.10), (3, 2.00);
         SELECT group_concat(price) FROM (SELECT price FROM t ORDER BY price);",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("200,990,1010".to_string())], "2.00, 9.90, 10.10");
}

/// `round(numeric, n)` becomes integer arithmetic, which is where R35's `trunc`
/// fix has to agree: both round away from zero on a negative operand, measured
/// on PostgreSQL 16 as `round(-2.5, 0) = -3` and `round(1.005, 2) = 1.01`.
#[test]
fn round_matches_postgres_on_both_signs() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, v NUMERIC(10,3));
         INSERT INTO t VALUES (1, 2.500), (2, -2.500), (3, 1.005);
         SELECT round(v, 0) FROM t ORDER BY id;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(
        rows,
        vec![Some("3000".to_string()), Some("-3000".to_string()), Some("1000".to_string())],
        "3, -3, and 1, still at the column's scale of 3"
    );
}

/// A consumer reading `price` gets 1999 where PostgreSQL gave 19.99, so the
/// representation has to be discoverable rather than guessed. The manifest
/// already describes the logical-to-physical mapping, so it carries the scale.
#[test]
fn the_manifest_publishes_the_scale() {
    let manifest = Pg2Sqlite::default()
        .sql("CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(10,2), note TEXT);")
        .expect("parse")
        .translation_manifest(&Pg2SqliteOptions::default())
        .expect("manifest");

    let table = manifest.iter().find(|entry| entry.logical == "t").expect("the table");
    let scaled: Vec<_> = table
        .columns
        .iter()
        .filter_map(|column| column.minor_unit_scale.map(|scale| (column.name.as_str(), scale)))
        .collect();
    assert_eq!(scaled, vec![("price", 2)], "only the NUMERIC column carries a scale");
}

/// An `UPDATE` writes into the same scaled column an `INSERT` does, so the same
/// literal has to mean the same thing. Measured on PostgreSQL 16: after
/// `UPDATE ... SET price = 1.50` the column reads 1.50, and after
/// `SET price = 3` it reads 3.00.
#[test]
fn a_literal_is_scaled_on_update() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(10,2));
         INSERT INTO t VALUES (1, 2.00), (2, 2.00), (3, 2.00);
         UPDATE t SET price = 1.50 WHERE id = 1;
         UPDATE t SET price = 3 WHERE id = 2;
         UPDATE t SET price = -1.50 WHERE id = 3;
         SELECT price FROM t ORDER BY id;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(
        rows,
        vec![Some("150".to_string()), Some("300".to_string()), Some("-150".to_string())],
        "1.50, 3.00 and -1.50 in minor units"
    );
}

/// The tuple spelling of the same assignment. SQLite accepts
/// `SET (a, b) = (1, 2)`, measured on 3.51.1, so the only thing that can go
/// wrong with it is the scaling.
#[test]
fn a_literal_is_scaled_in_a_tuple_assignment() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT, price NUMERIC(10,2));
         INSERT INTO t VALUES (1, 0, 2.00);
         UPDATE t SET (n, price) = (7, 1.50) WHERE id = 1;
         SELECT n || ':' || price FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(
        rows,
        vec![Some("7:150".to_string())],
        "the plain column is untouched, the scaled one moves"
    );
}

/// The upsert's assignment list is the same write through a different door.
#[test]
fn a_literal_is_scaled_in_an_upsert_assignment() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(10,2));
         INSERT INTO t VALUES (1, 2.00);
         INSERT INTO t VALUES (1, 9.99) ON CONFLICT (id) DO UPDATE SET price = 1.50;
         SELECT price FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("150".to_string())], "the DO UPDATE literal is scaled too");
}

/// A maintenance trigger body is an `UPDATE` this crate builds itself, so it
/// needs the same treatment rather than its own.
#[test]
fn a_literal_is_scaled_in_a_trigger_row_assignment() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT, price NUMERIC(10,2));
         CREATE FUNCTION f() RETURNS TRIGGER AS $$
         BEGIN
             NEW.price := 1.50;
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER tr BEFORE UPDATE ON t FOR EACH ROW EXECUTE FUNCTION f();
         INSERT INTO t VALUES (1, 0, 2.00);
         UPDATE t SET n = 1 WHERE id = 1;
         SELECT price FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("150".to_string())], "the trigger's literal is scaled");
}

/// The same refusal the insert path gives, so the two doors answer alike.
#[test]
fn a_literal_finer_than_the_column_is_refused_on_update() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(10,2));
             UPDATE t SET price = 19.999 WHERE id = 1;",
        )
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("19.999 does not fit a scale of 2")
        .to_string();
    assert!(error.contains("19.999"), "the error must name the literal, got: {error}");
}

/// Guards the fix rather than testing it. Arithmetic is scaled by the
/// expression translator, which already brings a literal operand onto the
/// column's scale, so the new assignment-level scaling must leave it alone
/// rather than multiply it a second time.
#[test]
fn an_arithmetic_assignment_is_not_scaled_twice() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(10,2));
         INSERT INTO t VALUES (1, 2.00);
         UPDATE t SET price = price + 1 WHERE id = 1;
         SELECT price FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("300".to_string())], "2.00 plus 1 is 3.00, which is 300");
}

/// Guards the fix. A column with no scale must keep its literal untouched.
#[test]
fn a_plain_integer_column_is_not_scaled_on_update() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT);
         INSERT INTO t VALUES (1, 0);
         UPDATE t SET n = 3 WHERE id = 1;
         SELECT n FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("3".to_string())], "a plain INTEGER column stores 3");
}

/// The declared `DEFAULT` writes into the same scaled column every statement
/// does, so it has to land in minor units. Unscaled it either dies on the
/// first omitting insert, `cannot store REAL value in INTEGER column`, or
/// stores a hundredth of the PostgreSQL value.
#[test]
fn a_declared_default_is_scaled() {
    let rows = run_translated_with(
        "CREATE TABLE t (
             id INT PRIMARY KEY,
             a NUMERIC(10,2) DEFAULT 1.50,
             b NUMERIC(10,2) DEFAULT 5,
             c NUMERIC(10,2) DEFAULT -1.50
         );
         INSERT INTO t (id) VALUES (1);
         SELECT a || '|' || b || '|' || c FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("150|500|-150".to_string())], "1.50, 5.00 and -1.50 in minor units");
}

/// PostgreSQL coerces a quoted default, `DEFAULT '1.50'` reads back 1.50, and
/// a parenthesised literal is the same literal, so both spellings scale.
/// Measured on PostgreSQL 16.
#[test]
fn a_quoted_or_parenthesised_default_is_scaled() {
    let rows = run_translated_with(
        "CREATE TABLE t (
             id INT PRIMARY KEY,
             a NUMERIC(10,2) DEFAULT '1.50',
             b NUMERIC(10,2) DEFAULT (2.50)
         );
         INSERT INTO t (id) VALUES (1);
         SELECT a || '|' || b FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("150|250".to_string())], "both spellings are the literal");
}

/// Guards the fix. `DEFAULT NULL` is result-neutral and must survive, and a
/// scale-zero NUMERIC and a plain INTEGER take no scaling at all.
#[test]
fn null_and_unscaled_defaults_are_untouched() {
    let rows = run_translated_with(
        "CREATE TABLE t (
             id INT PRIMARY KEY,
             a NUMERIC(10,2) DEFAULT NULL,
             b NUMERIC(10,0) DEFAULT 5,
             c INT DEFAULT 7
         );
         INSERT INTO t (id) VALUES (1);
         SELECT coalesce(a, -1) || '|' || b || '|' || c FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("-1|5|7".to_string())], "NULL, 5 and 7 exactly as declared");
}

/// A default the translator cannot land as one number at the column's scale is
/// refused rather than guessed, the same rule as an over-precise literal.
/// PostgreSQL evaluates `(1.0 + 0.5)` to 1.50 at insert time, which minor
/// units cannot reproduce without evaluating arithmetic at translate time.
#[test]
fn a_computed_default_on_a_scaled_column_is_refused() {
    let error = Pg2Sqlite::default()
        .sql("CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(10,2) DEFAULT (1.0 + 0.5));")
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("a computed default cannot be scaled at translate time")
        .to_string();
    assert!(error.contains("price"), "the refusal must name the column, got: {error}");
}

/// The malformed quoted spelling PostgreSQL itself rejects at CREATE TABLE,
/// measured on PostgreSQL 16: invalid input syntax for type numeric.
#[test]
fn a_malformed_quoted_default_is_refused() {
    let error = Pg2Sqlite::default()
        .sql("CREATE TABLE t (id INT PRIMARY KEY, price NUMERIC(10,2) DEFAULT 'abc');")
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("PostgreSQL rejects this declaration too")
        .to_string();
    assert!(error.contains("price"), "the refusal must name the column, got: {error}");
}
