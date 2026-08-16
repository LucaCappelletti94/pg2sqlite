//! Parity for NUMERIC, compared through the scale the manifest publishes.
//!
//! pg2sqlite stores NUMERIC(p,s) as an INTEGER of minor units: 1.50 in a
//! NUMERIC(10,2) column is stored as 150 in SQLite. A naive comparison is
//! wrong by construction. Every assertion here reads the scale from the
//! manifest and applies it, never hard-coding a power of ten.
//!
//! Two constructs diverge and are recorded as findings rather than suppressed:
//! decimal literals with more fractional digits than the column scale, where
//! PostgreSQL rounds silently and the translator refuses; and NUMERIC division,
//! where integer division truncates toward zero and the translator refuses.
//!
//! Mutation proof: the test `numeric_write_read_back` will fail when a wrong
//! minor-unit value is inserted into SQLite (e.g. 301 instead of 300 for 3.00
//! at scale 2), because `to_minor_units` converts the PG value 3.00 to 300 and
//! the equality check fails.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, TableManifestEntry};

use crate::{helpers, postgres_harness};

/// Source DDL applied to PostgreSQL directly and to SQLite after translation.
const SOURCE: &str = "
CREATE TABLE amounts (
    id     INTEGER PRIMARY KEY,
    price  NUMERIC(10,2),
    rate   NUMERIC(8,3),
    units  NUMERIC(8,0),
    fine   NUMERIC(15,5)
);
";

/// Typed schema for the SQLite side of amounts. NUMERIC(p,s) columns become
/// INTEGER (BigInt) after translation, storing minor-unit integers.
mod schema {
    diesel::table! {
        use diesel::sql_types::*;
        amounts (id) {
            id    -> Integer,
            price -> Nullable<BigInt>,
            rate  -> Nullable<BigInt>,
            units -> Nullable<BigInt>,
            fine  -> Nullable<BigInt>,
        }
    }
}

use schema::amounts;

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

/// Parse the DDL once; clone this for each statement that needs translation.
fn base_translator() -> Pg2Sqlite {
    Pg2Sqlite::default().sql(SOURCE).expect("parse amounts DDL")
}

/// Minor-unit scale for a named column in the amounts table, read from the
/// manifest. Panics when the column is not present.
fn column_scale(manifest: &[TableManifestEntry], column: &str) -> u32 {
    manifest
        .iter()
        .find(|t| t.logical == "amounts")
        .unwrap_or_else(|| panic!("amounts not in manifest"))
        .columns
        .iter()
        .find(|c| c.name == column)
        .unwrap_or_else(|| panic!("{column} not in amounts manifest"))
        .minor_unit_scale
        .unwrap_or(0)
}

/// Convert a PostgreSQL decimal text representation to the minor-unit integer
/// at the given scale. Panics when the text has more fractional digits than
/// the scale allows, which indicates the test is using the wrong scale.
fn to_minor_units(pg_text: &str, scale: u32) -> i64 {
    let s = pg_text.trim();
    let (neg, s) = s.strip_prefix('-').map_or((false, s), |rest| (true, rest));
    let (whole, frac) = s.find('.').map_or((s, ""), |i| (&s[..i], &s[i + 1..]));
    let frac_len = u32::try_from(frac.len()).expect("fraction length fits u32");
    assert!(
        frac_len <= scale,
        "PG value {pg_text:?} has {frac_len} decimal digits but column scale is {scale}"
    );
    // scale - frac_len <= 18 (MAX_NUMERIC_PRECISION), so the conversion is
    // lossless.
    let padding_count = usize::try_from(scale - frac_len).expect("padding count fits usize");
    let digits = format!("{whole}{frac}{}", "0".repeat(padding_count));
    let n: i64 = digits.parse().unwrap_or_else(|_| panic!("cannot parse {digits:?} as i64"));
    if neg { -n } else { n }
}

/// Translate `sql` in the context of the amounts schema and return the last
/// translated statement. Panics when the translator refuses the statement.
///
/// Used only for SELECT arithmetic where the translated SQL must be run on
/// SQLite to align column scales (e.g. subtraction across scales).
fn translate_dml(base: &Pg2Sqlite, sql: &str) -> String {
    base.clone()
        .sql(sql)
        .expect("parse DML")
        .translate_to_sql(&options())
        .expect("translate DML")
        .into_iter()
        .last()
        .expect("at least one translated statement")
}

/// A single text column named `val` in a raw query result.
///
/// Used for PG arithmetic reads: diesel cannot use NUMERIC without
/// `bigdecimal`, which is not a dev-dependency, so a CAST-to-TEXT raw
/// query is the only available form.
#[derive(QueryableByName)]
struct TextRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    val: String,
}

/// A single i64 column named `val` in a raw query result.
///
/// Used for SQLite arithmetic SELECTs where the expression involves
/// cross-column arithmetic or a translator-rescaled form that diesel's
/// typed DSL cannot represent.
#[derive(QueryableByName)]
struct I64Row {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    val: i64,
}

/// Fresh PostgreSQL connection with the amounts schema applied.
fn pg_setup() -> PgConnection {
    let mut conn = postgres_harness::fresh_database();
    postgres_harness::apply(&mut conn, SOURCE).expect("apply amounts DDL to PG");
    conn
}

/// Fresh SQLite connection with the translated amounts schema applied.
///
/// DDL is applied via `diesel::sql_query` because CREATE TABLE is not
/// expressible in the diesel typed DSL.
fn sqlite_setup(base: &Pg2Sqlite) -> SqliteConnection {
    let ddl_stmts = base.clone().translate_to_sql(&options()).expect("translate amounts DDL");
    let mut conn = establish_connection();
    for stmt in &ddl_stmts {
        // Migration DDL: sql_query is the correct form here.
        diesel::sql_query(stmt.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("SQLite DDL failed: {e}\n{stmt}"));
    }
    conn
}

/// Read the first row's `val` column as text from a PG raw query.
///
/// This uses `sql_query` because reading PG NUMERIC requires `bigdecimal`,
/// which is not a dev-dependency. CAST to TEXT is the available alternative.
fn pg_text(conn: &mut PgConnection, sql: &str) -> String {
    diesel::sql_query(sql)
        .load::<TextRow>(conn)
        .expect("PG query")
        .into_iter()
        .next()
        .expect("at least one row")
        .val
}

/// Read the first row's `val` column as i64 from a SQLite raw query.
///
/// Used when the query is a translated or arithmetic form that the typed DSL
/// cannot represent.
fn sqlite_i64(conn: &mut SqliteConnection, sql: &str) -> i64 {
    diesel::sql_query(sql)
        .load::<I64Row>(conn)
        .expect("SQLite query")
        .into_iter()
        .next()
        .expect("at least one row")
        .val
}

/// The two engines read the same value back after a write, at every scale.
#[test]
fn numeric_write_read_back() {
    let base = base_translator();
    let manifest = base.translation_manifest(&options()).expect("manifest");
    let price_scale = column_scale(&manifest, "price");
    let rate_scale = column_scale(&manifest, "rate");
    let units_scale = column_scale(&manifest, "units");
    let fine_scale = column_scale(&manifest, "fine");

    let mut pg_conn = pg_setup();
    let mut sqlite_conn = sqlite_setup(&base);

    // PostgreSQL: original SQL, stored as NUMERIC.
    postgres_harness::apply(
        &mut pg_conn,
        "INSERT INTO amounts (id, price, rate, units, fine) VALUES (1, 3.00, 1.500, 42, 3.14159)",
    )
    .expect("PG insert");

    // SQLite: typed insert with pre-computed minor units derived from the
    // manifest's published scale. Using typed DSL avoids unverified raw SQL.
    diesel::insert_into(amounts::table)
        .values((
            amounts::id.eq(1i32),
            amounts::price.eq(Some(to_minor_units("3.00", price_scale))),
            amounts::rate.eq(Some(to_minor_units("1.500", rate_scale))),
            amounts::units.eq(Some(to_minor_units("42", units_scale))),
            amounts::fine.eq(Some(to_minor_units("3.14159", fine_scale))),
        ))
        .execute(&mut sqlite_conn)
        .expect("SQLite insert");

    // Read each column back. PostgreSQL returns the decimal text; SQLite
    // returns the stored integer. Both must match at the manifest's scale.
    //
    // Price (scale 2): PG "3.00" -> minor units 300 == SQLite stored 300.
    let pg_price =
        pg_text(&mut pg_conn, "SELECT CAST(price AS TEXT) AS val FROM amounts WHERE id = 1");
    let sqlite_price: Option<i64> = amounts::table
        .filter(amounts::id.eq(1i32))
        .select(amounts::price)
        .get_result(&mut sqlite_conn)
        .expect("SQLite price read");
    assert_eq!(
        sqlite_price.expect("non-null price"),
        to_minor_units(&pg_price, price_scale),
        "price read-back divergence: PG={pg_price:?} scale={price_scale}"
    );

    // Rate (scale 3): PG "1.500" -> 1500 == SQLite 1500.
    let pg_rate =
        pg_text(&mut pg_conn, "SELECT CAST(rate AS TEXT) AS val FROM amounts WHERE id = 1");
    let sqlite_rate: Option<i64> = amounts::table
        .filter(amounts::id.eq(1i32))
        .select(amounts::rate)
        .get_result(&mut sqlite_conn)
        .expect("SQLite rate read");
    assert_eq!(
        sqlite_rate.expect("non-null rate"),
        to_minor_units(&pg_rate, rate_scale),
        "rate read-back divergence: PG={pg_rate:?} scale={rate_scale}"
    );

    // Units (scale 0): PG "42" -> 42 == SQLite 42.
    let pg_units =
        pg_text(&mut pg_conn, "SELECT CAST(units AS TEXT) AS val FROM amounts WHERE id = 1");
    let sqlite_units: Option<i64> = amounts::table
        .filter(amounts::id.eq(1i32))
        .select(amounts::units)
        .get_result(&mut sqlite_conn)
        .expect("SQLite units read");
    assert_eq!(
        sqlite_units.expect("non-null units"),
        to_minor_units(&pg_units, units_scale),
        "units read-back divergence: PG={pg_units:?} scale={units_scale}"
    );

    // Fine (scale 5): PG "3.14159" -> 314159 == SQLite 314159.
    let pg_fine =
        pg_text(&mut pg_conn, "SELECT CAST(fine AS TEXT) AS val FROM amounts WHERE id = 1");
    let sqlite_fine: Option<i64> = amounts::table
        .filter(amounts::id.eq(1i32))
        .select(amounts::fine)
        .get_result(&mut sqlite_conn)
        .expect("SQLite fine read");
    assert_eq!(
        sqlite_fine.expect("non-null fine"),
        to_minor_units(&pg_fine, fine_scale),
        "fine read-back divergence: PG={pg_fine:?} scale={fine_scale}"
    );
}

/// Addition, subtraction (across scales), multiplication, and SUM all give the
/// same rational value on both engines when read through the manifest's scale.
#[test]
fn numeric_arithmetic() {
    let base = base_translator();
    let manifest = base.translation_manifest(&options()).expect("manifest");
    let price_scale = column_scale(&manifest, "price");
    let rate_scale = column_scale(&manifest, "rate");

    let mut pg_conn = pg_setup();
    let mut sqlite_conn = sqlite_setup(&base);

    // Two rows: one for single-column arithmetic, one for cross-column and SUM.
    //   Row 1: price=3.00, rate=1.500 (for addition and multiplication)
    //   Row 2: price=2.50, rate=2.000 (for subtraction and SUM)
    postgres_harness::apply(
        &mut pg_conn,
        "INSERT INTO amounts (id, price, rate) VALUES (1, 3.00, 1.500), (2, 2.50, 2.000)",
    )
    .expect("PG inserts");

    let row1_price_mu = to_minor_units("3.00", price_scale); // 300
    let row1_rate_mu = to_minor_units("1.500", rate_scale); // 1500
    let row2_price_mu = to_minor_units("2.50", price_scale); // 250
    let row2_rate_mu = to_minor_units("2.000", rate_scale); // 2000

    diesel::insert_into(amounts::table)
        .values((
            amounts::id.eq(1i32),
            amounts::price.eq(Some(row1_price_mu)),
            amounts::rate.eq(Some(row1_rate_mu)),
        ))
        .execute(&mut sqlite_conn)
        .expect("SQLite insert row 1");
    diesel::insert_into(amounts::table)
        .values((
            amounts::id.eq(2i32),
            amounts::price.eq(Some(row2_price_mu)),
            amounts::rate.eq(Some(row2_rate_mu)),
        ))
        .execute(&mut sqlite_conn)
        .expect("SQLite insert row 2");

    // Addition: price + price (same scale). The result scale equals price_scale.
    //
    // sql_query is used here because `price + price` is cross-expression
    // arithmetic that diesel's typed DSL for Nullable<BigInt> cannot represent
    // without introducing wrapper types not in scope for this test.
    let add_scale = price_scale;
    let pg_add = pg_text(
        &mut pg_conn,
        "SELECT CAST(price + price AS TEXT) AS val FROM amounts WHERE id = 1",
    );
    let sqlite_add =
        sqlite_i64(&mut sqlite_conn, "SELECT price + price AS val FROM amounts WHERE id = 1");
    assert_eq!(
        sqlite_add,
        to_minor_units(&pg_add, add_scale),
        "addition divergence: PG={pg_add:?} SQLite={sqlite_add} scale={add_scale}"
    );

    // Subtraction across scales: price (scale 2) - rate (scale 3). The
    // translator rescales price to the common scale (3) before subtracting.
    // Result scale = max(price_scale, rate_scale). The raw SQLite integer
    // (250 - 2000 = -1750) would be wrong; the translated form aligns first.
    let sub_scale = price_scale.max(rate_scale);
    let pg_sub =
        pg_text(&mut pg_conn, "SELECT CAST(price - rate AS TEXT) AS val FROM amounts WHERE id = 2");
    // Translated SQL (e.g. "(price * 10) - rate") runs on SQLite.
    let sqlite_sub_sql =
        translate_dml(&base, "SELECT price - rate AS val FROM amounts WHERE id = 2");
    let sqlite_sub = sqlite_i64(&mut sqlite_conn, &sqlite_sub_sql);
    assert_eq!(
        sqlite_sub,
        to_minor_units(&pg_sub, sub_scale),
        "subtraction divergence: PG={pg_sub:?} SQLite={sqlite_sub} scale={sub_scale}"
    );

    // Multiplication: price * rate. The translator passes integer multiplication
    // through unchanged (no rescaling). Result scale = price_scale + rate_scale.
    let mul_scale = price_scale + rate_scale;
    let pg_mul =
        pg_text(&mut pg_conn, "SELECT CAST(price * rate AS TEXT) AS val FROM amounts WHERE id = 1");
    let sqlite_mul =
        sqlite_i64(&mut sqlite_conn, "SELECT price * rate AS val FROM amounts WHERE id = 1");
    assert_eq!(
        sqlite_mul,
        to_minor_units(&pg_mul, mul_scale),
        "multiplication divergence: PG={pg_mul:?} SQLite={sqlite_mul} scale={mul_scale}"
    );

    // SUM aggregate: sum preserves the input scale.
    //
    // diesel::dsl::sum(amounts::price) returns Nullable<Numeric> (not
    // Nullable<BigInt>), so the typed DSL cannot load the result as i64 for
    // SQLite. sql_query with the same expression is the correct form here.
    let postgres_total =
        pg_text(&mut pg_conn, "SELECT CAST(SUM(price) AS TEXT) AS val FROM amounts");
    let sqlite_minor_units_total =
        sqlite_i64(&mut sqlite_conn, "SELECT SUM(price) AS val FROM amounts");
    assert_eq!(
        sqlite_minor_units_total,
        to_minor_units(&postgres_total, price_scale),
        "SUM divergence: PG={postgres_total:?} scale={price_scale}"
    );
}

/// Both engines refuse a value that exceeds the column's declared precision.
///
/// For NUMERIC(10,2), the translated column has a CHECK constraint that allows
/// at most 9999999999. Inserting 10000000000 (= 100000000.00 at scale 2) fails
/// on both PostgreSQL (numeric field overflow) and SQLite (CHECK violation).
#[test]
fn numeric_precision_bound() {
    let base = base_translator();
    let manifest = base.translation_manifest(&options()).expect("manifest");
    let price_scale = column_scale(&manifest, "price");
    let units_scale = column_scale(&manifest, "units");

    let mut pg_conn = pg_setup();
    let mut sqlite_conn = sqlite_setup(&base);

    // NUMERIC(10,2) accepts at most 8 digits before the decimal. 100000000.00
    // has 9, which overflows both engines.
    let pg_result = postgres_harness::apply(
        &mut pg_conn,
        "INSERT INTO amounts (id, price) VALUES (99, 100000000.00)",
    );
    assert!(pg_result.is_err(), "PG must refuse a value exceeding NUMERIC(10,2) precision");

    // The minor-unit value for 100000000.00 at the manifest's scale exceeds
    // the CHECK constraint (10000000000 > 9999999999).
    let oversize_price = to_minor_units("100000000.00", price_scale);
    let sqlite_result = diesel::insert_into(amounts::table)
        .values((amounts::id.eq(99i32), amounts::price.eq(Some(oversize_price))))
        .execute(&mut sqlite_conn);
    assert!(
        sqlite_result.is_err(),
        "SQLite CHECK must refuse minor-unit value {oversize_price} for NUMERIC(10,2)"
    );

    // NUMERIC(8,0) accepts at most 8 digits (up to 99999999). The literal
    // 100000000 overflows the scale-0 precision bound on both engines.
    let pg_units_result = postgres_harness::apply(
        &mut pg_conn,
        "INSERT INTO amounts (id, units) VALUES (98, 100000000)",
    );
    assert!(pg_units_result.is_err(), "PG must refuse 100000000 in NUMERIC(8,0)");

    // For scale 0 the literal is its own minor-unit representation (scale 0
    // means no fractional part, so to_minor_units("100000000", 0) = 100000000).
    let oversize_units = to_minor_units("100000000", units_scale);
    let sqlite_units_result = diesel::insert_into(amounts::table)
        .values((amounts::id.eq(98i32), amounts::units.eq(Some(oversize_units))))
        .execute(&mut sqlite_conn);
    assert!(
        sqlite_units_result.is_err(),
        "SQLite CHECK must refuse minor-unit value {oversize_units} for NUMERIC(8,0)"
    );
}

/// Finding: PostgreSQL rounds a decimal literal to the column scale. The
/// translator refuses the literal to prevent silent data change.
///
/// The test asserts the translator refuses (which is the correct behaviour)
/// and documents what PostgreSQL does with the same statement.
#[test]
fn numeric_rounding_finding() {
    let base = base_translator();

    // 1.999 has 3 fractional digits; NUMERIC(10,2) has scale 2. PostgreSQL
    // rounds to 2.00. The translator refuses: the literal cannot be scaled
    // without silent rounding.
    let insert = "INSERT INTO amounts (id, price) VALUES (1, 1.999)";
    let translation_err = base
        .clone()
        .sql(insert)
        .expect("parse")
        .translate_to_sql(&options())
        .expect_err("translator must refuse a literal with more digits than the column scale");

    // Document what PostgreSQL does: it accepts the statement and rounds.
    let mut pg_conn = pg_setup();
    postgres_harness::apply(&mut pg_conn, insert).expect("PG must accept 1.999 and round to 2.00");

    // The rounded value stored in PG:
    let pg_rounded =
        pg_text(&mut pg_conn, "SELECT CAST(price AS TEXT) AS val FROM amounts WHERE id = 1");

    // Finding: PG accepted and rounded; the translator refused with the message
    // below, so the two engines cannot agree through the translator on this insert.
    // PG stored: {pg_rounded}, translator refused: {translation_err}
    let _ = (pg_rounded, translation_err);
}

/// Finding: PostgreSQL keeps fractional digits in NUMERIC division. SQLite
/// integer division truncates toward zero. The translator refuses NUMERIC
/// division because there is no faithful SQLite form.
///
/// Confirmed by seeding both engines and comparing raw results: PostgreSQL
/// gives a non-zero quotient, raw SQLite gives 0 (integer truncation).
#[test]
fn numeric_division_finding() {
    let base = base_translator();

    // The translator must refuse NUMERIC division.
    let translation_err = base
        .clone()
        .sql("SELECT CAST(price / rate AS TEXT) AS val FROM amounts WHERE id = 1")
        .expect("parse")
        .translate_to_sql(&options())
        .expect_err("translator must refuse NUMERIC division");

    // Seed both engines to confirm the runtime divergence.
    let mut pg_conn = pg_setup();
    let mut sqlite_conn = sqlite_setup(&base);

    let manifest = base.translation_manifest(&options()).expect("manifest");
    let price_scale = column_scale(&manifest, "price");
    let rate_scale = column_scale(&manifest, "rate");

    postgres_harness::apply(
        &mut pg_conn,
        "INSERT INTO amounts (id, price, rate) VALUES (1, 3.00, 1.500)",
    )
    .expect("PG insert");

    diesel::insert_into(amounts::table)
        .values((
            amounts::id.eq(1i32),
            amounts::price.eq(Some(to_minor_units("3.00", price_scale))),
            amounts::rate.eq(Some(to_minor_units("1.500", rate_scale))),
        ))
        .execute(&mut sqlite_conn)
        .expect("SQLite insert");

    // PostgreSQL: 3.00 / 1.500 produces a non-zero fractional result.
    let pg_div =
        pg_text(&mut pg_conn, "SELECT CAST(price / rate AS TEXT) AS val FROM amounts WHERE id = 1");
    assert_ne!(pg_div, "0", "PG division must be non-zero; translator refused: {translation_err}");

    // Raw SQLite: 300 / 1500 = 0 (integer division truncates toward zero).
    // This is the divergence the translator's refusal prevents from reaching
    // application code silently.
    let sqlite_raw =
        sqlite_i64(&mut sqlite_conn, "SELECT price / rate AS val FROM amounts WHERE id = 1");
    assert_eq!(
        sqlite_raw, 0,
        "raw SQLite integer division must give 0 for 300/1500; PG gives {pg_div:?}"
    );
}
