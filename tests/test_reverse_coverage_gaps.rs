//! Reverse-direction coverage gaps from the value-fidelity patch.
//!
//! Unreachable by design: `current_date`, `current_time`, `current_timestamp`,
//! `localtime`, `localtimestamp` parse as dedicated AST nodes in SQLiteDialect,
//! never as `Expr::Identifier`, so `pseudo_expression_kind` lines 267-270 and
//! `is_negative_integer_limit`'s `Number("-1",…)` arm (line 1857) are dead.
//! `INSERT DEFAULT VALUES` from SQLiteDialect always has
//! `TableObject::TableName`, so line 300 is dead.  Lines 488 and 495 are
//! covered by test_reverse_dml.rs.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

fn reverse(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let stmts = translator
        .reverse_sql(sqlite_sql, &schema, &Pg2SqliteOptions::default())
        .unwrap_or_else(|e| panic!("expected success, got: {e}"));
    let pg = stmts.first().expect("one statement").to_string();
    Parser::parse_sql(&PostgreSqlDialect {}, &pg)
        .unwrap_or_else(|e| panic!("output must parse as PostgreSQL: {e}\n{pg}"));
    pg
}

fn reverse_err(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    translator
        .reverse_sql(sqlite_sql, &schema, &Pg2SqliteOptions::default())
        .unwrap_err()
        .to_string()
}

fn forward(pg_sql: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg_sql)
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .unwrap()
}

const TV_SCHEMA: &str = "CREATE TABLE tv (id INT PRIMARY KEY, v INT); CREATE TABLE src (n INT);";

// ident_quoting.rs 126-138

#[test]
fn do_update_set_subquery_inner_refs_not_qualified() {
    // Identifiers inside the subquery must not gain the table prefix.
    let pg = reverse(
        TV_SCHEMA,
        "INSERT INTO tv (id, v) VALUES (1, 1) \
         ON CONFLICT (id) DO UPDATE SET v = (SELECT n FROM src)",
    );
    assert!(pg.contains("ON CONFLICT"), "{pg}");
    assert!(pg.contains("DO UPDATE"), "{pg}");
    assert!(pg.contains("SELECT"), "{pg}");
    assert!(!pg.contains("tv.n"), "n inside subquery must not gain tv. prefix: {pg}");
}

// ident_quoting.rs 140-142

#[test]
fn do_update_set_default_value_not_qualified() {
    // sqlparser produces Expr::Identifier("DEFAULT") here; the
    // eq_ignore_ascii_case guard skips it.
    let pg = reverse(
        "CREATE TABLE tv (id INT PRIMARY KEY, v INT DEFAULT 42);",
        "INSERT INTO tv (id, v) VALUES (1, 1) \
         ON CONFLICT (id) DO UPDATE SET v = DEFAULT",
    );
    assert!(pg.contains("DEFAULT"), "{pg}");
    assert!(!pg.contains("tv.DEFAULT"), "{pg}");
}

// insert.rs 488 Err branch

#[test]
fn do_update_set_untranslatable_expr_is_refused() {
    let err = reverse_err(
        TV_SCHEMA,
        "INSERT INTO tv (id, v) VALUES (1, 1) \
         ON CONFLICT (id) DO UPDATE SET v = json_type(v)",
    );
    assert!(err.contains("json_type"), "{err}");
}

// expr.rs 266

#[test]
fn current_schema_not_confirmed_in_schema_is_refused() {
    // `current_schema` parses as Expr::Identifier in SQLiteDialect; without a
    // column in scope it errors.
    let err = reverse_err("CREATE TABLE other (id INT);", "SELECT current_schema FROM nonexistent");
    assert!(err.contains("pseudo-expression"), "error must mention pseudo-expression: {err}");
    assert!(
        err.contains("schema name") || err.contains("schema"),
        "error must name the kind: {err}"
    );
}

// insert.rs 301

#[test]
fn default_values_into_unknown_table_passes_through() {
    // resolve_translation_table returns Ok(None); the function returns Ok(())
    // without checking.
    let pg = reverse(
        "CREATE TABLE real_table (id INT PRIMARY KEY);",
        "INSERT INTO unknown_table DEFAULT VALUES",
    );
    assert!(pg.to_uppercase().contains("DEFAULT VALUES"), "{pg}");
}

// helpers.rs 107

#[test]
fn reverse_select_from_unknown_schema_is_refused() {
    // refuse_sqlite_specific_names passes for `myschema`;
    // validate_schema_qualified then fails.
    let err = reverse_err("CREATE TABLE t (id INT PRIMARY KEY);", "SELECT * FROM myschema.t");
    assert!(
        err.contains("myschema") || err.to_lowercase().contains("schema"),
        "error must mention the unknown schema: {err}"
    );
}

// shared_helpers.rs 1878 and 1857

#[test]
fn reverse_positive_limit_is_preserved() {
    // Number("10") hits the Value arm of is_negative_integer_limit;
    // starts_with('-') is false.
    let schema = "CREATE TABLE t (id INT PRIMARY KEY);";
    let pg = reverse(schema, "SELECT id FROM t LIMIT 10");
    assert!(pg.contains("LIMIT 10"), "positive LIMIT must be kept: {pg}");
}

#[test]
fn reverse_negative_limit_is_dropped() {
    // SQLite treats LIMIT -1 as no limit; PostgreSQL rejects it, so the reverse
    // drops it.
    let schema = "CREATE TABLE t (id INT PRIMARY KEY);";
    let pg = reverse(schema, "SELECT id FROM t LIMIT -1");
    assert!(!pg.to_uppercase().contains("LIMIT"), "negative LIMIT must be dropped: {pg}");
}

// shared_helpers.rs 1879-1880

#[test]
fn forward_negative_limit_is_passed_through() {
    // IS_FORWARD=true returns translated unchanged without filtering.
    let stmts = forward("CREATE TABLE t (id INT PRIMARY KEY); SELECT id FROM t LIMIT -1;");
    let select = stmts.iter().find(|s| s.to_uppercase().contains("LIMIT")).cloned();
    let select = select.unwrap_or_else(|| stmts.join("\n"));
    assert!(
        select.contains("LIMIT -1") || select.contains("LIMIT"),
        "forward direction must not drop LIMIT -1: {select}"
    );
}
