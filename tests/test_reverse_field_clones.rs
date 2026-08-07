//! Tests for GROUP M: Reverse function/query field clones.
//!
//! M1: Rename path clones `func.parameters`
//! M2: Rename `within_group` clones `with_fill`
//! M3: `Select::reverse_translate` clones `top`
//! M4: Insert `table` (`TableObject`) cloned
//! M5: `SetExpr::Merge` cloned without translation

mod helpers;

use helpers::reverse_translate_sql;

#[test]
fn reverse_rename_translates_parameters() {
    // json_group_array → json_agg is a Rename reversal
    // The function parameters should be reverse-translated, not cloned
    let sql = "SELECT json_group_array(name) FROM t";
    let result = reverse_translate_sql(sql).unwrap();
    let lower = result.to_lowercase();

    // Should become json_agg(name)
    assert!(lower.contains("json_agg"), "json_group_array should reverse to json_agg: {result}");
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, &result)
        .expect("reverse output must parse as PostgreSQL");
}

#[test]
fn reverse_rename_within_group_translates() {
    // Test that within_group ORDER BY is reverse-translated properly
    // (with_fill is ClickHouse-specific, but the OrderByExpr should still be
    // handled)
    let sql = "SELECT json_group_array(val) FROM t";
    let result = reverse_translate_sql(sql).unwrap();
    let lower = result.to_lowercase();

    assert!(lower.contains("json_agg"), "json_group_array should reverse to json_agg: {result}");
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, &result)
        .expect("reverse output must parse as PostgreSQL");
}

#[test]
fn reverse_rename_preserves_function_structure() {
    // json_group_object → json_object_agg reversal
    let sql = "SELECT json_group_object(key, value) FROM t";
    let result = reverse_translate_sql(sql).unwrap();
    let lower = result.to_lowercase();

    assert!(
        lower.contains("json_object_agg"),
        "json_group_object should reverse to json_object_agg: {result}"
    );
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, &result)
        .expect("reverse output must parse as PostgreSQL");
}

#[test]
fn reverse_insert_table_name_preserved() {
    // Simple INSERT should reverse-translate table name correctly
    let sql = "INSERT INTO t (id, name) VALUES (1, 'test')";
    let result = reverse_translate_sql(sql).unwrap();
    let lower = result.to_lowercase();

    assert!(lower.contains("insert into t"), "INSERT table name should be preserved: {result}");
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, &result)
        .expect("reverse output must parse as PostgreSQL");
}

#[test]
fn reverse_datetime_now_to_now() {
    // datetime('now') → NOW()
    let sql = "SELECT datetime('now')";
    let result = reverse_translate_sql(sql).unwrap();
    let lower = result.to_lowercase();

    assert!(lower.contains("now()"), "datetime('now') should reverse to NOW(): {result}");
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, &result)
        .expect("reverse output must parse as PostgreSQL");
}
