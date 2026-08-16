//! Parity for aggregates, JSON, arrays and uuid.
//!
//! One table with integer, double, text, JSONB, TEXT[] and UUID columns,
//! seeded identically on both engines. Each test compares one or more
//! constructs across both engines and asserts they agree.
//!
//! Typed diesel DSL is used for every aggregate the DSL can express:
//! count, sum, min, max over the integer and text columns. `diesel::sql_query`
//! is used for constructs the DSL genuinely cannot express:
//!
//! - avg: PG returns numeric, SQLite returns real; no shared Rust type.
//! - string_agg, array_agg, cardinality, array subscript, unnest: absent from
//!   diesel DSL.
//! - JSON operators (->, ->>, json_typeof, jsonb_set): absent from diesel DSL.
//! - Statistical aggregates (var_pop, var_samp, stddev, corr): absent from
//!   diesel DSL.
//! - UUID equality comparison: UUID on PG vs BLOB on SQLite; no shared diesel
//!   column type for a cross-backend comparison.
//!
//! The statistical-aggregates test additionally opens a rusqlite Connection
//! for aggregate registration; diesel provides no API for window-aggregate
//! registration (create_window_function is a rusqlite API).
//!
//! Floating point comparisons use a relative tolerance of FLOAT_TOL (1e-9),
//! which covers last-digit rounding ("0.6666666666666666" vs
//! "0.6666666666666667") and the PG/SQLite text-representation difference
//! where PG emits "1" and SQLite emits "1.0" for the same 1.0 value.

// Window-aggregate registration requires a rusqlite::Connection; there is no
// diesel equivalent.
#[path = "../helpers/statistical_aggregates.rs"]
mod stat_agg;

use std::fmt::Write;

use diesel::{dsl, pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{
    ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions, TranslationOptions, UuidRepresentation,
};
use rusqlite::Connection as RusqliteConn;
use stat_agg::{STATISTICAL_AGGREGATES, register_statistical_aggregates};

use crate::{helpers::establish_connection, postgres_harness};

// ── typed diesel schema
// ───────────────────────────────────────────────────────
//
// Only columns used in typed DSL queries appear here. Columns with
// backend-specific types (JSONB, TEXT[], UUID) are omitted; every query that
// touches them goes through sql_query.
//
// n and s are declared non-nullable in the schema even though the actual rows
// can be NULL: the schema drives type inference for the aggregate expressions,
// and NULL rows flow through MIN/MAX/SUM/COUNT correctly at the SQL level.

diesel::table! {
    vals (id) {
        id -> Integer,
        n -> Integer,
        s -> Text,
    }
}

// ── PostgreSQL source
// ─────────────────────────────────────────────────────────

// DOUBLE PRECISION so the diesel schema can declare r -> Double (f64 on both
// backends) if a future typed query needs it. The translator emits REAL for
// SQLite, which is also 64-bit.
const SCHEMA: &str = "
CREATE TABLE vals (
    id      INTEGER PRIMARY KEY,
    n       INTEGER,
    r       DOUBLE PRECISION,
    s       TEXT,
    payload JSONB,
    tags    TEXT[],
    uid     UUID
);
";

// Row 3 has NULL for n/r/s/payload and an empty TEXT[] for tags, exercising
// NULL and empty-collection edge cases in every aggregate.
const SEEDS: &str = r#"
INSERT INTO vals VALUES (1, 10, 1.5, 'hello', '{"a":1,"b":"x"}', ARRAY['foo','bar'], '11111111-1111-1111-1111-111111111111');
INSERT INTO vals VALUES (2, 20, 2.5, 'world', '[1,2,3]', ARRAY['baz'], '22222222-2222-2222-2222-222222222222');
INSERT INTO vals VALUES (3, NULL, NULL, NULL, NULL, ARRAY[]::TEXT[], '33333333-3333-3333-3333-333333333333');
INSERT INTO vals VALUES (4, 30, 3.5, 'hello', '{"a":2}', ARRAY['foo','baz'], '44444444-4444-4444-4444-444444444444');
"#;

// ── translation options
// ───────────────────────────────────────────────────────

fn values_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_array_representation(ArrayRepresentation::Json)
        .with_user_defined_functions(STATISTICAL_AGGREGATES.iter().copied())
}

// ── setup ─────────────────────────────────────────────────────────────────────

fn setup_pg() -> PgConnection {
    let mut conn = postgres_harness::fresh_database();
    postgres_harness::apply(&mut conn, &format!("{SCHEMA}{SEEDS}"))
        .expect("apply schema and seeds to PostgreSQL");
    conn
}

// Translates SCHEMA+SEEDS and applies via diesel::sql_query. Using sql_query
// for DDL and seed inserts is the correct path: the statements are translator
// output (migration DDL), and the seed values contain PostgreSQL-specific
// syntax (ARRAY[], JSONB literals, UUID) that has no unified typed-insert
// representation across both backends.
fn setup_sq_diesel() -> SqliteConnection {
    let source = format!("{SCHEMA}{SEEDS}");
    let stmts = Pg2Sqlite::default()
        .sql(&source)
        .expect("parse schema and seeds")
        .translate_to_sql(&values_options())
        .expect("translate schema and seeds");

    // establish_connection configures foreign_keys, recursive_triggers,
    // and UUID functions; none of the latter interfere with this schema.
    let mut conn = establish_connection();
    for stmt in &stmts {
        // Migration DDL: sql_query is correct for translator-emitted schema.
        diesel::sql_query(stmt.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("SQLite setup: {e}\n{stmt}"));
    }
    conn
}

// Returns a rusqlite Connection with statistical aggregates registered. Used
// only by `statistical_aggregates_agree_within_tolerance` because diesel
// provides no API for window-aggregate registration.
fn setup_sq_rusqlite() -> RusqliteConn {
    let source = format!("{SCHEMA}{SEEDS}");
    let stmts = Pg2Sqlite::default()
        .sql(&source)
        .expect("parse schema and seeds")
        .translate_to_sql(&values_options())
        .expect("translate schema and seeds");

    let sq = RusqliteConn::open_in_memory().expect("in-memory SQLite");
    sq.execute_batch("PRAGMA foreign_keys = ON; PRAGMA recursive_triggers = ON;")
        .expect("configure SQLite pragmas");
    register_statistical_aggregates(&sq).expect("register statistical aggregates");
    for stmt in &stmts {
        sq.execute_batch(&format!("{stmt};"))
            .unwrap_or_else(|e| panic!("SQLite setup: {e}\n{stmt}"));
    }
    sq
}

// ── raw-SQL helpers
// ───────────────────────────────────────────────────────────
//
// Used only for constructs the typed DSL cannot express. Every query aliases
// its projection AS val and casts non-text results to text (e.g.
// count(*)::text) so the QueryableByName struct can load any value as
// Option<String>.

#[derive(QueryableByName)]
struct Val {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    val: Option<String>,
}

fn pg_sql(conn: &mut PgConnection, sql: &str) -> Vec<Option<String>> {
    diesel::sql_query(sql)
        .load::<Val>(conn)
        .unwrap_or_else(|e| panic!("PG query failed: {e}\n{sql}"))
        .into_iter()
        .map(|r| r.val)
        .collect()
}

// Translates pg_query (SCHEMA provides type context for column lookups), then
// runs the last emitted statement on the SQLite connection.
fn sq_sql_diesel(conn: &mut SqliteConnection, pg_query: &str) -> Vec<Option<String>> {
    let full = format!("{SCHEMA}\n{pg_query}");
    let mut stmts = Pg2Sqlite::default()
        .sql(&full)
        .expect("parse query")
        .translate_to_sql(&values_options())
        .expect("translate query");
    let probe = stmts.pop().expect("translation produced at least one statement");
    diesel::sql_query(probe.as_str())
        .load::<Val>(conn)
        .unwrap_or_else(|e| panic!("SQLite query failed: {e}\n{probe}"))
        .into_iter()
        .map(|r| r.val)
        .collect()
}

// Translates and runs on a rusqlite Connection (for statistical aggregates).
fn sq_sql_rusqlite(sq: &RusqliteConn, pg_query: &str) -> Vec<Option<String>> {
    let full = format!("{SCHEMA}\n{pg_query}");
    let mut stmts = Pg2Sqlite::default()
        .sql(&full)
        .expect("parse query")
        .translate_to_sql(&values_options())
        .expect("translate query");
    let probe = stmts.pop().expect("translation produced at least one statement");
    let mut stmt =
        sq.prepare(&probe).unwrap_or_else(|e| panic!("SQLite prepare failed: {e}\n{probe}"));
    stmt.query_map([], |row| {
        use rusqlite::types::ValueRef;
        Ok(match row.get_ref(0)? {
            ValueRef::Null => None,
            ValueRef::Integer(i) => Some(i.to_string()),
            ValueRef::Real(f) => Some(f.to_string()),
            ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(b) => {
                Some(b.iter().fold(String::new(), |mut hex, byte| {
                    let _ = write!(hex, "{byte:02x}");
                    hex
                }))
            }
        })
    })
    .unwrap_or_else(|e| panic!("SQLite query failed: {e}\n{probe}"))
    .collect::<Result<Vec<_>, _>>()
    .expect("SQLite rows")
}

// ── comparison helpers
// ────────────────────────────────────────────────────────

const FLOAT_TOL: f64 = 1e-9;

fn float_close(a: f64, b: f64) -> bool {
    (a - b).abs() <= FLOAT_TOL * a.abs().max(b.abs()).max(1.0)
}

fn values_agree(pg: Option<&String>, sq: Option<&String>) -> bool {
    match (pg, sq) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            if a == b {
                return true;
            }
            match (a.parse::<f64>(), b.parse::<f64>()) {
                (Ok(fa), Ok(fb)) => float_close(fa, fb),
                _ => false,
            }
        }
        _ => false,
    }
}

fn assert_sql_rows(pg: &[Option<String>], sq: &[Option<String>], label: &str) {
    assert_eq!(pg.len(), sq.len(), "{label}: row count pg={} sq={}", pg.len(), sq.len());
    for (i, (p, s)) in pg.iter().zip(sq.iter()).enumerate() {
        assert!(values_agree(p.as_ref(), s.as_ref()), "{label}[{i}]: pg={p:?} sq={s:?}");
    }
}

fn assert_sql_scalar(pg: &[Option<String>], sq: &[Option<String>], label: &str) {
    assert_eq!(pg.len(), 1, "{label}: expected one PG row, got {}", pg.len());
    assert_eq!(sq.len(), 1, "{label}: expected one SQLite row, got {}", sq.len());
    assert!(values_agree(pg[0].as_ref(), sq[0].as_ref()), "{label}: pg={:?} sq={:?}", pg[0], sq[0]);
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[test]
fn scalar_aggregates_treat_null_and_empty_set_consistently() {
    let mut pg = setup_pg();
    let mut sq = setup_sq_diesel();

    // count(*): typed diesel; same i64 on both backends.
    let pg_count = vals::table.count().get_result::<i64>(&mut pg).expect("count pg");
    let sq_count = vals::table.count().get_result::<i64>(&mut sq).expect("count sq");
    assert_eq!(pg_count, sq_count, "count all rows");

    // count(n IS NOT NULL): typed filter + count; skips row 3 (NULL n).
    let pg_n = vals::table
        .filter(vals::n.is_not_null())
        .count()
        .get_result::<i64>(&mut pg)
        .expect("count n pg");
    let sq_n = vals::table
        .filter(vals::n.is_not_null())
        .count()
        .get_result::<i64>(&mut sq)
        .expect("count n sq");
    assert_eq!(pg_n, sq_n, "count non-null n");

    // sum(n): typed; PG bigint and SQLite integer both arrive as Option<i64>.
    let pg_sum =
        vals::table.select(dsl::sum(vals::n)).get_result::<Option<i64>>(&mut pg).expect("sum pg");
    let sq_sum =
        vals::table.select(dsl::sum(vals::n)).get_result::<Option<i64>>(&mut sq).expect("sum sq");
    assert_eq!(pg_sum, sq_sum, "sum n");

    // min/max(n): typed; both return Option<i32>.
    let pg_min =
        vals::table.select(dsl::min(vals::n)).get_result::<Option<i32>>(&mut pg).expect("min n pg");
    let sq_min =
        vals::table.select(dsl::min(vals::n)).get_result::<Option<i32>>(&mut sq).expect("min n sq");
    assert_eq!(pg_min, sq_min, "min n");

    let pg_max =
        vals::table.select(dsl::max(vals::n)).get_result::<Option<i32>>(&mut pg).expect("max n pg");
    let sq_max =
        vals::table.select(dsl::max(vals::n)).get_result::<Option<i32>>(&mut sq).expect("max n sq");
    assert_eq!(pg_max, sq_max, "max n");

    // min/max(s): typed; both return Option<String>. NULLs excluded by MIN/MAX.
    let pg_mins = vals::table
        .select(dsl::min(vals::s))
        .get_result::<Option<String>>(&mut pg)
        .expect("min s pg");
    let sq_mins = vals::table
        .select(dsl::min(vals::s))
        .get_result::<Option<String>>(&mut sq)
        .expect("min s sq");
    assert_eq!(pg_mins, sq_mins, "min s");

    let pg_maxs = vals::table
        .select(dsl::max(vals::s))
        .get_result::<Option<String>>(&mut pg)
        .expect("max s pg");
    let sq_maxs = vals::table
        .select(dsl::max(vals::s))
        .get_result::<Option<String>>(&mut sq)
        .expect("max s sq");
    assert_eq!(pg_maxs, sq_maxs, "max s");

    // avg(n): sql_query because PG returns numeric and SQLite returns real;
    // there is no Rust type that diesel can load from both backends without a
    // backend-specific cast.
    let q = "SELECT avg(n)::text AS val FROM vals";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "avg n");

    // empty set: typed count returns 0, typed sum returns NULL.
    let pg_ec = vals::table
        .filter(vals::id.lt(0))
        .count()
        .get_result::<i64>(&mut pg)
        .expect("empty count pg");
    let sq_ec = vals::table
        .filter(vals::id.lt(0))
        .count()
        .get_result::<i64>(&mut sq)
        .expect("empty count sq");
    assert_eq!(pg_ec, sq_ec, "count empty set");

    let pg_empty_sum = vals::table
        .filter(vals::id.lt(0))
        .select(dsl::sum(vals::n))
        .get_result::<Option<i64>>(&mut pg)
        .expect("empty sum pg");
    let sqlite_empty_sum = vals::table
        .filter(vals::id.lt(0))
        .select(dsl::sum(vals::n))
        .get_result::<Option<i64>>(&mut sq)
        .expect("empty sum sq");
    assert_eq!(pg_empty_sum, sqlite_empty_sum, "sum empty set");

    let q = "SELECT avg(n)::text AS val FROM vals WHERE id < 0";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "avg empty set");
}

#[test]
fn string_agg_with_ordering_agrees() {
    // string_agg with ORDER BY: sql_query; diesel has no string_agg DSL.
    let mut pg = setup_pg();
    let mut sq = setup_sq_diesel();

    // NULLs excluded by string_agg; ORDER BY s gives hello,hello,world.
    let q = "SELECT string_agg(s, ',' ORDER BY s) AS val FROM vals";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "string_agg ordered");

    // Empty set yields NULL on both.
    let q = "SELECT string_agg(s, ',' ORDER BY s) AS val FROM vals WHERE id < 0";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "string_agg empty");
}

#[test]
fn array_agg_element_count_agrees() {
    // array_agg: sql_query; diesel has no array_agg DSL. The raw text
    // representation differs between engines (PG emits {10,20,NULL,30},
    // SQLite emits [10,20,null,30]), so cardinality is compared instead of
    // the raw aggregate.
    let mut pg = setup_pg();
    let mut sq = setup_sq_diesel();

    let q = "SELECT cardinality(array_agg(n))::text AS val FROM vals";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "array_agg all");

    let q = "SELECT cardinality(array_agg(n))::text AS val FROM vals WHERE n IS NOT NULL";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "array_agg non-null");
}

#[test]
fn statistical_aggregates_agree_within_tolerance() {
    // rusqlite is used because register_statistical_aggregates requires a
    // rusqlite::Connection; diesel provides no API for window-aggregate
    // registration (create_window_function is a rusqlite API).
    //
    // r values (NULL in row 3 excluded): 1.5, 2.5, 3.5
    // var_pop = 2/3; var_samp = 1.0; stddev = 1.0
    // n=[10,20,30] and r=[1.5,2.5,3.5] are perfectly linearly correlated: corr=1.0
    let mut pg = setup_pg();
    let sq = setup_sq_rusqlite();

    for (q, label) in [
        ("SELECT var_pop(r)::text AS val FROM vals", "var_pop"),
        ("SELECT var_samp(r)::text AS val FROM vals", "var_samp"),
        ("SELECT stddev(r)::text AS val FROM vals", "stddev"),
        ("SELECT corr(n, r)::text AS val FROM vals", "corr"),
        // Empty set: every statistical aggregate is NULL.
        ("SELECT var_pop(r)::text AS val FROM vals WHERE id < 0", "var_pop empty"),
        ("SELECT corr(n, r)::text AS val FROM vals WHERE id < 0", "corr empty"),
    ] {
        assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_rusqlite(&sq, q), label);
    }
}

#[test]
fn json_extraction_agrees() {
    // JSON operators: sql_query; diesel has no -> / ->> DSL for JSONB columns.
    let mut pg = setup_pg();
    let mut sq = setup_sq_diesel();

    // -> returns the JSON value; ::text gives its JSON text representation.
    // payload for id=1: {"a":1,"b":"x"} -- key 'a' is integer 1.
    let q = "SELECT (payload->'a')::text AS val FROM vals WHERE id = 1";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "-> integer key");

    // ->> returns text, stripping JSON encoding (so string values lose quotes).
    let q = "SELECT payload->>'b' AS val FROM vals WHERE id = 1";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "->>");

    // NULL payload propagates NULL.
    let q = "SELECT (payload->'a')::text AS val FROM vals WHERE id = 3";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "-> null payload");
}

#[test]
fn json_typeof_agrees() {
    // json_typeof: sql_query; the translator normalises vocabulary to PostgreSQL
    // names ('object', 'array', 'number', 'string', 'boolean', 'null').
    // Row 3 has NULL payload so json_typeof returns NULL.
    let mut pg = setup_pg();
    let mut sq = setup_sq_diesel();

    let q = "SELECT jsonb_typeof(payload) AS val FROM vals ORDER BY id";
    assert_sql_rows(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "json_typeof");
}

#[test]
fn jsonb_set_agrees() {
    // jsonb_set: sql_query; diesel has no jsonb_set DSL. Chaining ->> focuses
    // on the updated value, avoiding the whitespace difference between
    // PostgreSQL's {"a": 99, "b": "x"} and SQLite's {"a":99,"b":"x"}.
    let mut pg = setup_pg();
    let mut sq = setup_sq_diesel();

    let q = "SELECT (jsonb_set(payload, '{a}', '99'))->>'a' AS val FROM vals WHERE id = 1";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "jsonb_set existing key");

    // Create a missing key 'c' with value 42.
    let q = "SELECT (jsonb_set(payload, '{c}', '42'))->>'c' AS val FROM vals WHERE id = 1";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "jsonb_set new key");
}

#[test]
fn array_indexing_agrees() {
    // Array subscript: sql_query; diesel has no subscript DSL for array
    // columns. PG is 1-indexed; the translator maps to 0-indexed json_extract.
    // tags for id=1: ['foo','bar'].
    let mut pg = setup_pg();
    let mut sq = setup_sq_diesel();

    let q = "SELECT tags[1] AS val FROM vals WHERE id = 1";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "tags[1]");

    let q = "SELECT tags[2] AS val FROM vals WHERE id = 1";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "tags[2]");

    // Index past the end of an empty array (id=3, tags=[]) returns NULL on both.
    let q = "SELECT tags[1] AS val FROM vals WHERE id = 3";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "tags[1] empty array");
}

#[test]
fn array_length_agrees() {
    // cardinality: sql_query; diesel has no cardinality / json_array_length DSL.
    // Row 3 has an empty array, giving 0.
    let mut pg = setup_pg();
    let mut sq = setup_sq_diesel();

    let q = "SELECT cardinality(tags)::text AS val FROM vals ORDER BY id";
    assert_sql_rows(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "cardinality by row");
}

#[test]
fn unnest_from_literal_agrees() {
    // unnest: sql_query; diesel has no unnest DSL. unnest over a column
    // reference is rejected by the translator; a literal array in FROM is the
    // supported path. ORDER BY makes the comparison deterministic.
    let mut pg = setup_pg();
    let mut sq = setup_sq_diesel();

    let q = "SELECT x AS val FROM unnest(ARRAY['c','a','b']) AS t(x) ORDER BY x";
    assert_sql_rows(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "unnest literal");
}

#[test]
fn uuid_round_trip_agrees() {
    let mut pg = setup_pg();
    let mut sq = setup_sq_diesel();

    // Typed diesel count confirms all four seed rows arrived on both engines.
    let pg_total = vals::table.count().get_result::<i64>(&mut pg).expect("total count pg");
    let sq_total = vals::table.count().get_result::<i64>(&mut sq).expect("total count sq");
    assert_eq!(pg_total, sq_total, "total row count");

    // UUID IS NOT NULL: sql_query because the uid column type is UUID on PG
    // and BLOB on SQLite; there is no shared diesel column type that can be
    // declared in a single table! covering both backends.
    let q = "SELECT count(*)::text AS val FROM vals WHERE uid IS NOT NULL";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "uuid not null count");

    // UUID equality lookup: the translator converts the UUID literal to the
    // BLOB comparison SQLite needs under UuidRepresentation::Blob.
    let q = "SELECT count(*)::text AS val FROM vals \
             WHERE uid = '11111111-1111-1111-1111-111111111111'::uuid";
    assert_sql_scalar(&pg_sql(&mut pg, q), &sq_sql_diesel(&mut sq, q), "uuid equality");
}
