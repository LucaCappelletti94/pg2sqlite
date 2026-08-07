//! Focused tests for operator translation gaps: arithmetic, regex, ROW
//! constructor, and JSON operators.
//!
//! Each translated case asserts the exact emitted SQL and, where the result
//! does not depend on SQLITE_ENABLE_MATH_FUNCTIONS, executes the translated
//! SQL against an in-memory connection to verify the numeric or JSON value.
//!
//! Math-dependent cases (pow, sqrt) assert the SQL string only. Executing
//! them would require SQLITE_ENABLE_MATH_FUNCTIONS or a registered UDF,
//! which would test the UDF rather than the translator.

mod helpers;

use diesel::{QueryableByName, prelude::*, sql_query, sql_types};
use helpers::{establish_connection, translate_pg};
use pg2sqlite::prelude::{ArrayRepresentation, Pg2SqliteOptions, TranslationOptions};

fn opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_array_representation(ArrayRepresentation::Json)
        .with_math_functions_available()
}

/// Translate `pg` with all options on and return the single emitted statement.
fn tr(pg: &str) -> String {
    translate_pg(pg, &opts())
        .expect("translation must succeed")
        .into_iter()
        .next()
        .expect("at least one statement")
}

/// Translate `pg` with all options on and expect a translation error.
fn tr_err(pg: &str) -> pg2sqlite::errors::Error {
    translate_pg(pg, &opts()).expect_err("translation must fail")
}

/// Scalar float result fetched from a `diesel::sql_query` with column alias
/// `r`.
///
/// `sql_query` is the right tool here: the SQL under test is the dynamic
/// output of pg2sqlite, not a statically known schema query.
#[derive(QueryableByName)]
struct ScalarDouble {
    #[diesel(sql_type = sql_types::Double)]
    r: f64,
}

/// Scalar integer result fetched from a `diesel::sql_query` with column alias
/// `r`.
///
/// See `ScalarDouble` for the justification for `sql_query`.
#[derive(QueryableByName)]
struct ScalarInt {
    #[diesel(sql_type = sql_types::Integer)]
    r: i32,
}

/// Scalar text result fetched from a `diesel::sql_query` with column alias `r`.
///
/// See `ScalarDouble` for the justification for `sql_query`.
#[derive(QueryableByName)]
struct ScalarText {
    #[diesel(sql_type = sql_types::Text)]
    r: String,
}

// ============================================================
// Arithmetic operators
// ============================================================

#[test]
fn caret_translates_to_pow_when_math_available() {
    // PostgreSQL uses ^ for exponentiation; SQLite uses ^ for bitwise XOR.
    // The translator must never pass it through unchanged.
    let sql = tr("SELECT 2 ^ 3");
    assert_eq!(sql, "SELECT pow(2, 3)", "unexpected emitted SQL: {sql}");
}

#[test]
fn caret_rejects_when_math_unavailable() {
    let opts = Pg2SqliteOptions::default();
    let err = translate_pg("SELECT 2 ^ 3", &opts).expect_err("must fail without math");
    let msg = err.to_string();
    assert!(
        msg.contains('^') || msg.contains("math") || msg.contains("pow"),
        "error must name ^ or math or pow: {msg}"
    );
}

#[test]
fn prefix_square_root_translates_to_sqrt() {
    // |/ x is PostgreSQL's prefix square-root operator.
    let sql = tr("SELECT |/ 9.0");
    assert_eq!(sql, "SELECT sqrt(9.0)", "unexpected emitted SQL: {sql}");
}

#[test]
fn prefix_square_root_rejects_when_math_unavailable() {
    let opts = Pg2SqliteOptions::default();
    let err = translate_pg("SELECT |/ 9.0", &opts).expect_err("must fail without math");
    let msg = err.to_string();
    assert!(msg.contains("|/") || msg.contains("math"), "error must name |/ or math: {msg}");
}

#[test]
fn prefix_cube_root_translates_to_pow_one_third() {
    // ||/ x is PostgreSQL's prefix cube-root operator.
    let sql = tr("SELECT ||/ 27.0");
    assert_eq!(sql, "SELECT pow(27.0, (1.0 / 3.0))", "unexpected emitted SQL: {sql}");
}

#[test]
fn prefix_cube_root_rejects_when_math_unavailable() {
    let opts = Pg2SqliteOptions::default();
    let err = translate_pg("SELECT ||/ 27.0", &opts).expect_err("must fail without math");
    let msg = err.to_string();
    assert!(msg.contains("||/") || msg.contains("math"), "error must name ||/ or math: {msg}");
}

// ============================================================
// R80 phase 2: operators that used to pass through untranslated
// ============================================================

/// `^@` is PostgreSQL's starts-with operator, and the passthrough emitted
/// `s ^@ 'a'`, which SQLite refuses with `unrecognized token: "^"`. It is
/// exact prefix comparison, no pattern semantics, so it becomes substr.
#[test]
fn starts_with_operator_translates_and_answers() {
    let sql = tr("SELECT 'abc' ^@ 'ab' AS r");
    let mut conn = establish_connection();
    let hit = sql_query(&sql).load::<ScalarInt>(&mut conn).expect("^@ must run");
    assert_eq!(hit[0].r, 1, "'abc' starts with 'ab': {sql}");

    let miss = tr("SELECT 'abc' ^@ 'b' AS r");
    let rows = sql_query(&miss).load::<ScalarInt>(&mut conn).expect("^@ must run");
    assert_eq!(rows[0].r, 0, "'abc' does not start with 'b': {miss}");

    // A LIKE wildcard in the prefix is literal for ^@, which is the trap a
    // LIKE-based translation would fall into.
    let literal = tr("SELECT 'abc' ^@ '%b' AS r");
    let rows = sql_query(&literal).load::<ScalarInt>(&mut conn).expect("^@ must run");
    assert_eq!(rows[0].r, 0, "the prefix is literal, not a pattern: {literal}");
}

/// `@?` is PostgreSQL's jsonpath-exists operator. json1 has no jsonpath
/// engine, so it is refused rather than emitted as the old passthrough,
/// which failed with `unrecognized token: "@"`.
#[test]
fn jsonpath_exists_operator_is_refused() {
    let err = tr_err("SELECT '{\"a\":1}'::jsonb @? '$.a'");
    let msg = err.to_string();
    assert!(msg.contains("@?"), "the refusal should name the operator: {msg}");
}

/// `OPERATOR(pg_catalog.+)` is PostgreSQL's schema-qualified operator
/// spelling for plain `+`. The passthrough emitted it verbatim, `near "(":
/// syntax error`. A known operator unwraps and rides its normal translation.
#[test]
fn operator_syntax_unwraps_known_operators() {
    let sql = tr("SELECT 2 OPERATOR(pg_catalog.+) 3 AS r");
    let mut conn = establish_connection();
    let rows = sql_query(&sql).load::<ScalarInt>(&mut conn).expect("unwrapped + must run");
    assert_eq!(rows[0].r, 5, "OPERATOR(pg_catalog.+) is plain addition: {sql}");
}

/// The unwrap re-dispatches through the normal operator translation, so a
/// wrapped operator keeps its specific handling: `^` becomes pow and `~`
/// keeps the POSIX regex refusal.
#[test]
fn operator_syntax_redispatches_through_operator_rules() {
    let sql = tr("SELECT 2 OPERATOR(pg_catalog.^) 3");
    assert_eq!(sql, "SELECT pow(2, 3)", "wrapped ^ must become pow: {sql}");

    let err = tr_err("SELECT 'a' OPERATOR(pg_catalog.~) 'b'");
    let msg = err.to_string();
    assert!(msg.contains("POSIX regex"), "wrapped ~ keeps the regex refusal: {msg}");
}

#[test]
fn pg_abs_prefix_translates_to_abs() {
    // @ x is PostgreSQL's prefix absolute-value operator; SQLite has abs().
    let sql = tr("SELECT @ -5");
    assert_eq!(sql, "SELECT abs(-5)", "unexpected emitted SQL: {sql}");
}

#[test]
fn pg_abs_executes_correctly() {
    let mut conn = establish_connection();
    // sql_query: executing dynamically generated SQL, not a static schema query.
    let result = sql_query("SELECT abs(-5) AS r")
        .get_result::<ScalarDouble>(&mut conn)
        .expect("execute abs")
        .r;
    assert!((result - 5.0).abs() < 1e-9, "abs(-5) should be 5.0, got {result}");
}

#[test]
fn pg_bitwise_xor_translates_to_or_minus_and() {
    // PostgreSQL # is bitwise XOR; SQLite has no # token.
    // Translation: (a | b) - (a & b) which equals a XOR b for integers.
    let sql = tr("SELECT 5 # 3");
    assert_eq!(sql, "SELECT (5 | 3) - (5 & 3)", "unexpected emitted SQL: {sql}");
}

#[test]
fn pg_bitwise_xor_executes_correctly() {
    let mut conn = establish_connection();
    // (5 | 3) - (5 & 3) = 7 - 1 = 6 = 5 XOR 3
    // sql_query: executing dynamically generated SQL, not a static schema query.
    let result = sql_query("SELECT (5 | 3) - (5 & 3) AS r")
        .get_result::<ScalarInt>(&mut conn)
        .expect("execute xor")
        .r;
    assert_eq!(result, 6, "(5 | 3) - (5 & 3) must equal 6 (5 XOR 3), got {result}");
}

// ============================================================
// Regex operators
// ============================================================

#[test]
fn regex_match_rejects() {
    // ~ requires POSIX regex, which SQLite does not provide natively.
    let err = tr_err("SELECT 'hello' ~ 'ell'");
    let msg = err.to_string();
    assert!(
        msg.contains("REGEXP") || msg.contains("regex") || msg.contains('~'),
        "error must name REGEXP or regex: {msg}"
    );
}

#[test]
fn regex_imatch_rejects_mentioning_case_insensitive() {
    // ~* is case-insensitive POSIX regex; SQLite REGEXP cannot honor that.
    let err = tr_err("SELECT 'Hello' ~* 'ell'");
    let msg = err.to_string();
    assert!(
        msg.contains("REGEXP") || msg.contains("regex"),
        "error must name REGEXP or regex: {msg}"
    );
    assert!(
        msg.to_lowercase().contains("case") || msg.contains("insensitive") || msg.contains("~*"),
        "error must mention case-insensitive for ~*: {msg}"
    );
}

#[test]
fn regex_not_match_rejects() {
    let err = tr_err("SELECT 'hello' !~ 'xyz'");
    let msg = err.to_string();
    assert!(msg.contains("REGEXP") || msg.contains("!~"), "error must name REGEXP or !~: {msg}");
}

#[test]
fn regex_not_imatch_rejects_mentioning_case_insensitive() {
    let err = tr_err("SELECT 'hello' !~* 'XYZ'");
    let msg = err.to_string();
    assert!(msg.contains("REGEXP") || msg.contains("!~*"), "error must name REGEXP or !~*: {msg}");
    assert!(
        msg.to_lowercase().contains("case") || msg.contains("insensitive") || msg.contains("!~*"),
        "error must mention case-insensitive for !~*: {msg}"
    );
}

// ============================================================
// ROW constructor
// ============================================================

#[test]
fn row_constructor_rejects_with_tuple_suggestion() {
    // ROW(a, b) as a value is unsupported; SQLite has no row type.
    let err = tr_err("SELECT ROW(1, 2)");
    let msg = err.to_string();
    assert!(msg.to_uppercase().contains("ROW"), "error must name ROW: {msg}");
    // The task requires naming the tuple-comparison workaround.
    assert!(
        msg.contains("tuple") || msg.contains("(a, b)") || msg.contains("comparison"),
        "error must suggest tuple comparison: {msg}"
    );
}

// ============================================================
// JSON operators
// ============================================================

#[test]
fn json_key_exists_translates_to_json_type_is_not_null() {
    // doc ? 'k' -> json_type(doc, '$."k"') IS NOT NULL
    let sql = tr(r#"SELECT '{"a":1}' ? 'a'"#);
    assert_eq!(
        sql, r#"SELECT json_type('{"a":1}', '$."a"') IS NOT NULL"#,
        "unexpected emitted SQL: {sql}"
    );
}

#[test]
fn json_key_exists_true_executes_correctly() {
    let mut conn = establish_connection();
    // sql_query: executing dynamically generated SQL, not a static schema query.
    let result = sql_query(r#"SELECT json_type('{"a":1}', '$."a"') IS NOT NULL AS r"#)
        .get_result::<ScalarInt>(&mut conn)
        .expect("execute key-exists true")
        .r;
    assert_eq!(result, 1, "key 'a' exists in {{\"a\":1}} so result must be 1");
}

#[test]
fn json_key_exists_false_executes_correctly() {
    let mut conn = establish_connection();
    // sql_query: executing dynamically generated SQL, not a static schema query.
    let result = sql_query(r#"SELECT json_type('{"a":1}', '$."b"') IS NOT NULL AS r"#)
        .get_result::<ScalarInt>(&mut conn)
        .expect("execute key-exists false")
        .r;
    assert_eq!(result, 0, "key 'b' not in {{\"a\":1}} so result must be 0");
}

#[test]
fn json_any_key_exists_translates_to_or_chain() {
    // doc ?| ARRAY['a','b'] -> OR chain of json_type IS NOT NULL
    let sql = tr(r#"SELECT '{"a":1}' ?| ARRAY['a', 'b']"#);
    assert_eq!(
        sql,
        r#"SELECT json_type('{"a":1}', '$."a"') IS NOT NULL OR json_type('{"a":1}', '$."b"') IS NOT NULL"#,
        "unexpected emitted SQL: {sql}"
    );
}

#[test]
fn json_any_key_exists_executes_correctly() {
    let mut conn = establish_connection();
    // 'a' exists, 'b' does not -> OR is true (1)
    // sql_query: executing dynamically generated SQL, not a static schema query.
    let result = sql_query(
        r#"SELECT json_type('{"a":1}', '$."a"') IS NOT NULL OR json_type('{"a":1}', '$."b"') IS NOT NULL AS r"#,
    )
    .get_result::<ScalarInt>(&mut conn)
    .expect("execute any-key OR")
    .r;
    assert_eq!(result, 1, "OR of key-checks must be 1 when any key exists");
}

#[test]
fn json_all_keys_exist_translates_to_and_chain() {
    // doc ?& ARRAY['a','b'] -> AND chain of json_type IS NOT NULL
    let sql = tr(r#"SELECT '{"a":1,"b":2}' ?& ARRAY['a', 'b']"#);
    assert_eq!(
        sql,
        r#"SELECT json_type('{"a":1,"b":2}', '$."a"') IS NOT NULL AND json_type('{"a":1,"b":2}', '$."b"') IS NOT NULL"#,
        "unexpected emitted SQL: {sql}"
    );
}

#[test]
fn json_all_keys_exist_executes_correctly() {
    let mut conn = establish_connection();
    // Both 'a' and 'b' exist -> AND is true (1)
    // sql_query: executing dynamically generated SQL, not a static schema query.
    let result = sql_query(
        r#"SELECT json_type('{"a":1,"b":2}', '$."a"') IS NOT NULL AND json_type('{"a":1,"b":2}', '$."b"') IS NOT NULL AS r"#,
    )
    .get_result::<ScalarInt>(&mut conn)
    .expect("execute all-keys AND")
    .r;
    assert_eq!(result, 1, "AND of key-checks must be 1 when all keys exist");
}

#[test]
fn json_delete_path_translates_to_json_remove() {
    // doc #- '{a}' -> json_remove(doc, '$.a')
    let sql = tr(r#"SELECT '{"a":1,"b":2}' #- '{a}'"#);
    assert_eq!(
        sql, r#"SELECT json_remove('{"a":1,"b":2}', '$.a')"#,
        "unexpected emitted SQL: {sql}"
    );
}

#[test]
fn json_delete_path_executes_correctly() {
    let mut conn = establish_connection();
    // sql_query: executing dynamically generated SQL, not a static schema query.
    let result = sql_query(r#"SELECT json_remove('{"a":1,"b":2}', '$.a') AS r"#)
        .get_result::<ScalarText>(&mut conn)
        .expect("execute json_remove")
        .r;
    assert_eq!(result, r#"{"b":2}"#, "json_remove must delete key 'a'");
}

#[test]
fn json_delete_nested_path_translates() {
    // #- with a two-element path: doc #- '{a,b}' -> json_remove(doc, '$.a.b')
    let sql = tr(r#"SELECT '{"a":{"b":1}}' #- '{a,b}'"#);
    assert_eq!(
        sql, r#"SELECT json_remove('{"a":{"b":1}}', '$.a.b')"#,
        "unexpected emitted SQL: {sql}"
    );
}

#[test]
fn json_containment_at_arrow_rejects() {
    // @> (jsonb containment) cannot be expressed in SQLite without a recursive CTE.
    let err = tr_err(r#"SELECT '{"a":1}' @> '{}'"#);
    let msg = err.to_string();
    assert!(
        msg.contains("@>") || msg.to_lowercase().contains("containment"),
        "error must mention @> or containment: {msg}"
    );
}

#[test]
fn json_contained_by_arrow_at_rejects() {
    // <@ (jsonb contained-by) same limitation.
    let err = tr_err(r#"SELECT '{}' <@ '{"a":1}'"#);
    let msg = err.to_string();
    assert!(
        msg.contains("<@")
            || msg.to_lowercase().contains("containment")
            || msg.to_lowercase().contains("contained"),
        "error must mention <@ or containment: {msg}"
    );
}
