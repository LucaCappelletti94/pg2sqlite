//! Tests for GROUP P: Forward function unsupported errors.
//!
//! These PG functions have no SQLite equivalent and should produce clear
//! errors.

mod helpers;

use helpers::translate_sql;
use pg2sqlite::prelude::Pg2SqliteOptions;

fn expect_unsupported(sql: &str) {
    let result = translate_sql(sql, &Pg2SqliteOptions::default());
    assert!(result.is_err(), "Expected unsupported error for: {sql}, got: {result:?}");
}

#[test]
fn regexp_split_to_array_unsupported() {
    expect_unsupported("SELECT regexp_split_to_array('hello world', '\\s+')");
}

#[test]
fn regexp_split_to_table_unsupported() {
    expect_unsupported("SELECT regexp_split_to_table('hello world', '\\s+')");
}

#[test]
fn string_to_array_unsupported() {
    expect_unsupported("SELECT string_to_array('a,b,c', ',')");
}

#[test]
fn quote_ident_unsupported() {
    expect_unsupported("SELECT quote_ident('hello')");
}

#[test]
fn convert_unsupported() {
    expect_unsupported("SELECT convert('hello', 'UTF8', 'LATIN1')");
}

#[test]
fn convert_from_unsupported() {
    expect_unsupported("SELECT convert_from('hello', 'UTF8')");
}

#[test]
fn convert_to_unsupported() {
    expect_unsupported("SELECT convert_to('hello', 'UTF8')");
}

#[test]
fn log_unsupported() {
    expect_unsupported("SELECT log(10)");
}

#[test]
fn ln_unsupported() {
    expect_unsupported("SELECT ln(2.718)");
}

#[test]
fn exp_unsupported() {
    expect_unsupported("SELECT exp(1)");
}

#[test]
fn sqrt_unsupported() {
    expect_unsupported("SELECT sqrt(16)");
}

#[test]
fn cbrt_unsupported() {
    expect_unsupported("SELECT cbrt(27)");
}

#[test]
fn power_unsupported() {
    expect_unsupported("SELECT power(2, 3)");
}

#[test]
fn sign_unsupported() {
    expect_unsupported("SELECT sign(-5)");
}

#[test]
fn factorial_unsupported() {
    expect_unsupported("SELECT factorial(5)");
}

#[test]
fn gcd_unsupported() {
    expect_unsupported("SELECT gcd(12, 8)");
}

#[test]
fn lcm_unsupported() {
    expect_unsupported("SELECT lcm(12, 8)");
}

#[test]
fn pi_unsupported() {
    expect_unsupported("SELECT pi()");
}

#[test]
fn degrees_unsupported() {
    expect_unsupported("SELECT degrees(3.14159)");
}

#[test]
fn radians_unsupported() {
    expect_unsupported("SELECT radians(180)");
}

#[test]
fn setseed_unsupported() {
    expect_unsupported("SELECT setseed(0.5)");
}

#[test]
fn width_bucket_unsupported() {
    expect_unsupported("SELECT width_bucket(5.35, 0.024, 10.06, 5)");
}

#[test]
fn make_timestamptz_unsupported() {
    expect_unsupported("SELECT make_timestamptz(2024, 1, 1, 12, 0, 0)");
}

#[test]
fn make_interval_unsupported() {
    expect_unsupported("SELECT make_interval(days => 10)");
}

#[test]
fn isfinite_unsupported() {
    expect_unsupported("SELECT isfinite(TIMESTAMP '2024-01-01')");
}

#[test]
fn justify_days_unsupported() {
    expect_unsupported("SELECT justify_days(INTERVAL '35 days')");
}

#[test]
fn justify_hours_unsupported() {
    expect_unsupported("SELECT justify_hours(INTERVAL '27 hours')");
}

#[test]
fn justify_interval_unsupported() {
    expect_unsupported("SELECT justify_interval(INTERVAL '1 month -1 hour')");
}

#[test]
fn timeofday_unsupported() {
    expect_unsupported("SELECT timeofday()");
}

#[test]
fn json_strip_nulls_unsupported() {
    expect_unsupported("SELECT json_strip_nulls('{\"a\": null}')");
}

#[test]
fn jsonb_strip_nulls_unsupported() {
    expect_unsupported("SELECT jsonb_strip_nulls('{\"a\": null}')");
}

#[test]
fn json_populate_record_unsupported() {
    expect_unsupported("SELECT json_populate_record(null::record, '{\"a\": 1}')");
}

#[test]
fn jsonb_populate_record_unsupported() {
    expect_unsupported("SELECT jsonb_populate_record(null::record, '{\"a\": 1}')");
}

#[test]
fn json_to_record_unsupported() {
    expect_unsupported("SELECT json_to_record('{\"a\": 1}')");
}

#[test]
fn jsonb_to_record_unsupported() {
    expect_unsupported("SELECT jsonb_to_record('{\"a\": 1}')");
}

#[test]
fn row_to_json_unsupported() {
    expect_unsupported("SELECT row_to_json(row(1, 'hello'))");
}

#[test]
fn array_append_unsupported() {
    expect_unsupported("SELECT array_append(ARRAY[1,2], 3)");
}

#[test]
fn array_cat_unsupported() {
    expect_unsupported("SELECT array_cat(ARRAY[1,2], ARRAY[3,4])");
}

#[test]
fn array_dims_unsupported() {
    expect_unsupported("SELECT array_dims(ARRAY[1,2])");
}

#[test]
fn array_fill_unsupported() {
    expect_unsupported("SELECT array_fill(0, ARRAY[3])");
}

#[test]
fn array_lower_unsupported() {
    expect_unsupported("SELECT array_lower(ARRAY[1,2], 1)");
}

#[test]
fn array_upper_unsupported() {
    expect_unsupported("SELECT array_upper(ARRAY[1,2], 1)");
}

#[test]
fn array_ndims_unsupported() {
    expect_unsupported("SELECT array_ndims(ARRAY[1,2])");
}

#[test]
fn array_position_unsupported() {
    expect_unsupported("SELECT array_position(ARRAY[1,2,3], 2)");
}

#[test]
fn array_positions_unsupported() {
    expect_unsupported("SELECT array_positions(ARRAY[1,2,1], 1)");
}

#[test]
fn array_prepend_unsupported() {
    expect_unsupported("SELECT array_prepend(0, ARRAY[1,2])");
}

#[test]
fn array_remove_unsupported() {
    expect_unsupported("SELECT array_remove(ARRAY[1,2,1], 1)");
}

#[test]
fn array_replace_unsupported() {
    expect_unsupported("SELECT array_replace(ARRAY[1,2], 2, 3)");
}

#[test]
fn cardinality_unsupported() {
    expect_unsupported("SELECT cardinality(ARRAY[1,2,3])");
}

#[test]
fn host_unsupported() {
    expect_unsupported("SELECT host('192.168.1.0/24')");
}

#[test]
fn abbrev_unsupported() {
    expect_unsupported("SELECT abbrev('192.168.1.0/24')");
}

#[test]
fn broadcast_unsupported() {
    expect_unsupported("SELECT broadcast('192.168.1.0/24')");
}

#[test]
fn family_unsupported() {
    expect_unsupported("SELECT family('192.168.1.0/24')");
}

#[test]
fn hostmask_unsupported() {
    expect_unsupported("SELECT hostmask('192.168.1.0/24')");
}

#[test]
fn masklen_unsupported() {
    expect_unsupported("SELECT masklen('192.168.1.0/24')");
}

#[test]
fn netmask_unsupported() {
    expect_unsupported("SELECT netmask('192.168.1.0/24')");
}

#[test]
fn network_unsupported() {
    expect_unsupported("SELECT network('192.168.1.0/24')");
}

#[test]
fn set_masklen_unsupported() {
    expect_unsupported("SELECT set_masklen('192.168.1.0/24', 16)");
}

#[test]
fn current_schemas_unsupported() {
    expect_unsupported("SELECT current_schemas(true)");
}

#[test]
fn has_table_privilege_unsupported() {
    expect_unsupported("SELECT has_table_privilege('user', 'table', 'SELECT')");
}

#[test]
fn obj_description_unsupported() {
    expect_unsupported("SELECT obj_description(12345, 'pg_class')");
}

#[test]
fn col_description_unsupported() {
    expect_unsupported("SELECT col_description(12345, 1)");
}

#[test]
fn pg_get_expr_unsupported() {
    expect_unsupported("SELECT pg_get_expr('node_tree', 12345)");
}

#[test]
fn pg_table_size_unsupported() {
    expect_unsupported("SELECT pg_table_size('my_table')");
}

#[test]
fn pg_total_relation_size_unsupported() {
    expect_unsupported("SELECT pg_total_relation_size('my_table')");
}

#[test]
fn pg_relation_size_unsupported() {
    expect_unsupported("SELECT pg_relation_size('my_table')");
}

#[test]
fn pg_column_size_unsupported() {
    expect_unsupported("SELECT pg_column_size(1)");
}

#[test]
fn pg_database_size_unsupported() {
    expect_unsupported("SELECT pg_database_size('mydb')");
}
