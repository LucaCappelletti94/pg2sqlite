//! Reverse translation must map SQLite bind placeholders to PostgreSQL numbered
//! parameters. SQLite accepts positional `?`, numbered `?N`, and the named
//! forms `:name`, `@name`, and `$name`, but PostgreSQL accepts only `$N`, so
//! reverse output that keeps a bare `?` is not executable parameterized
//! PostgreSQL. Numbering follows SQLite's own bind-index rule so a single bind
//! vector drives both the SQLite original and the PostgreSQL translation.

use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

const SCHEMA: &str = "CREATE TABLE t (a INT, b INT, c INT, d INT);";

fn reverse(sqlite_sql: &str) -> String {
    reverse_result(sqlite_sql).unwrap_or_else(|e| panic!("reverse translation failed: {e}"))
}

fn reverse_result(sqlite_sql: &str) -> Result<String, Error> {
    let translator = Pg2Sqlite::default().sql(SCHEMA).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options)?;
    Ok(stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
}

fn reparse_pg(sql: &str) {
    Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap_or_else(|e| {
        panic!("reverse output must reparse under PostgreSqlDialect: {e}\n{sql}")
    });
}

#[test]
fn bare_positionals_number_left_to_right() {
    let out = reverse("SELECT * FROM t WHERE a > ? AND b = ?");
    assert_eq!(out, "SELECT * FROM t WHERE a > $1 AND b = $2");
    assert!(!out.contains('?'), "{out}");
    reparse_pg(&out);
}

#[test]
fn numbered_placeholders_preserve_number() {
    let out = reverse("SELECT * FROM t WHERE a > ?2 AND b = ?1");
    assert_eq!(out, "SELECT * FROM t WHERE a > $2 AND b = $1");
    assert!(!out.contains('?'), "{out}");
    reparse_pg(&out);
}

#[test]
fn mixed_placeholders_follow_sqlite_assignment_rule() {
    // SQLite assigns a bare `?` one greater than the largest number used so
    // far. Starting at 0: first `?` -> 1, `?5` -> 5, trailing `?` -> 6.
    let out = reverse("SELECT * FROM t WHERE a > ? AND b = ?5 AND c = ?");
    assert_eq!(out, "SELECT * FROM t WHERE a > $1 AND b = $5 AND c = $6");
    assert!(!out.contains('?'), "{out}");
    reparse_pg(&out);
}

#[test]
fn placeholder_in_limit_and_offset() {
    let out = reverse("SELECT * FROM t LIMIT ? OFFSET ?");
    assert_eq!(out, "SELECT * FROM t LIMIT $1 OFFSET $2");
    assert!(!out.contains('?'), "{out}");
    reparse_pg(&out);
}

#[test]
fn placeholder_in_in_list() {
    let out = reverse("SELECT * FROM t WHERE a IN (?, ?)");
    assert_eq!(out, "SELECT * FROM t WHERE a IN ($1, $2)");
    assert!(!out.contains('?'), "{out}");
    reparse_pg(&out);
}

#[test]
fn placeholder_in_between_bounds() {
    let out = reverse("SELECT * FROM t WHERE a BETWEEN ? AND ?");
    assert_eq!(out, "SELECT * FROM t WHERE a BETWEEN $1 AND $2");
    assert!(!out.contains('?'), "{out}");
    reparse_pg(&out);
}

#[test]
fn placeholder_as_function_argument() {
    let out = reverse("SELECT length(?) FROM t");
    assert_eq!(out, "SELECT length($1) FROM t");
    assert!(!out.contains('?'), "{out}");
    reparse_pg(&out);
}

#[test]
fn placeholder_in_select_list_expression() {
    let out = reverse("SELECT a + ? FROM t");
    assert_eq!(out, "SELECT a + $1 FROM t");
    assert!(!out.contains('?'), "{out}");
    reparse_pg(&out);
}

#[test]
fn named_colon_placeholder_is_rejected() {
    let err = reverse_result("SELECT * FROM t WHERE a = :name").unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedNamedPlaceholder { .. }),
        "expected UnsupportedNamedPlaceholder, got: {err:?}"
    );
}

#[test]
fn named_at_placeholder_is_rejected() {
    let err = reverse_result("SELECT * FROM t WHERE a = @name").unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedNamedPlaceholder { .. }),
        "expected UnsupportedNamedPlaceholder, got: {err:?}"
    );
}

#[test]
fn named_dollar_placeholder_is_rejected() {
    let err = reverse_result("SELECT * FROM t WHERE a = $name").unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedNamedPlaceholder { .. }),
        "expected UnsupportedNamedPlaceholder, got: {err:?}"
    );
}

#[test]
fn dollar_numbered_placeholder_is_rejected_as_named() {
    // In SQLite `$1` is a named parameter (name "1") assigned a next-available
    // bind index, not the numbered `?1` form, so it cannot be mapped to a
    // PostgreSQL `$1` and is rejected. This also keeps a forward-then-reverse
    // round trip honest: forward emits `?N`, never `$N`.
    let err = reverse_result("SELECT * FROM t WHERE a = $1").unwrap_err();
    assert!(
        matches!(&err, Error::UnsupportedNamedPlaceholder { placeholder } if placeholder == "$1"),
        "expected UnsupportedNamedPlaceholder for $1, got: {err:?}"
    );
}

#[test]
fn named_placeholder_mixed_with_valid_ones_still_errors() {
    // A valid `?` preceding a named form must not yield a half-translated
    // statement: the whole translation fails.
    let err = reverse_result("SELECT * FROM t WHERE a = ? AND b = :name").unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedNamedPlaceholder { .. }),
        "expected UnsupportedNamedPlaceholder, got: {err:?}"
    );
}

#[test]
fn placeholders_coexist_with_backticked_identifiers() {
    // diesel emits backticked identifiers and bare placeholders together, so
    // both normalizations must apply in one pass.
    let out = reverse("SELECT `t`.`a` FROM `t` WHERE `t`.`a` > ?");
    assert_eq!(out, r#"SELECT "t"."a" FROM "t" WHERE "t"."a" > $1"#);
    assert!(!out.contains('?') && !out.contains('`'), "{out}");
    reparse_pg(&out);
}

#[test]
fn statement_without_placeholders_is_unchanged() {
    // Regression guard: a placeholder-free statement must translate exactly as
    // it did before placeholder handling existed.
    let out = reverse("SELECT `t`.`a` FROM `t` WHERE `t`.`a` > 1 ORDER BY `t`.`a`");
    assert_eq!(out, r#"SELECT "t"."a" FROM "t" WHERE "t"."a" > 1 ORDER BY "t"."a""#);
    reparse_pg(&out);
}

#[test]
fn every_translated_output_reparses_as_postgres_without_question_marks() {
    let corpus = [
        "SELECT * FROM t WHERE a > ? AND b = ?",
        "SELECT * FROM t WHERE a > ?2 AND b = ?1",
        "SELECT * FROM t WHERE a > ? AND b = ?5 AND c = ?",
        "SELECT * FROM t LIMIT ? OFFSET ?",
        "SELECT * FROM t WHERE a IN (?, ?)",
        "SELECT * FROM t WHERE a BETWEEN ? AND ?",
        "SELECT length(?) FROM t",
        "SELECT a + ? FROM t",
        "SELECT `t`.`a` FROM `t` WHERE `t`.`a` > ?",
        "UPDATE t SET a = ? WHERE b = ?",
        "DELETE FROM t WHERE a = ?",
        "INSERT INTO t (a, b) VALUES (?, ?)",
    ];
    for sql in corpus {
        let out = reverse(sql);
        assert!(!out.contains('?'), "question mark leaked for {sql}: {out}");
        reparse_pg(&out);
    }
}

#[test]
fn duplicate_numbered_placeholder_reuses_the_same_parameter() {
    // SQLite binds every `?1` to parameter 1, so both map to `$1`.
    let out = reverse("SELECT * FROM t WHERE a = ?1 AND b = ?1");
    assert_eq!(out, "SELECT * FROM t WHERE a = $1 AND b = $1");
    reparse_pg(&out);
}

#[test]
fn bare_then_numbered_can_collapse_to_one_parameter() {
    // The bare `?` is index 1 (0 + 1); the following `?1` is also index 1, so
    // both bind to `$1`, matching SQLite's assignment.
    let out = reverse("SELECT * FROM t WHERE a = ? AND b = ?1");
    assert_eq!(out, "SELECT * FROM t WHERE a = $1 AND b = $1");
    reparse_pg(&out);
}

#[test]
fn bare_after_higher_number_continues_from_the_max() {
    let out = reverse("SELECT * FROM t WHERE a = ?3 AND b = ? AND c = ?");
    assert_eq!(out, "SELECT * FROM t WHERE a = $3 AND b = $4 AND c = $5");
    reparse_pg(&out);
}

#[test]
fn placeholders_in_subquery_number_in_source_order() {
    // The subquery placeholder is textually first, so it takes $1 even though a
    // clause-order walk could reach the outer predicate first.
    let out = reverse("SELECT * FROM t WHERE a IN (SELECT a FROM t WHERE b = ?) AND c = ?");
    assert_eq!(out, "SELECT * FROM t WHERE a IN (SELECT a FROM t WHERE b = $1) AND c = $2");
    reparse_pg(&out);
}

#[test]
fn placeholders_in_cte_number_in_source_order() {
    let out = reverse("WITH x AS (SELECT a FROM t WHERE b = ?) SELECT * FROM x WHERE a = ?");
    assert_eq!(out, "WITH x AS (SELECT a FROM t WHERE b = $1) SELECT * FROM x WHERE a = $2");
    reparse_pg(&out);
}

#[test]
fn placeholder_in_having_clause() {
    let out = reverse("SELECT a FROM t GROUP BY a HAVING count(*) > ?");
    assert_eq!(out, "SELECT a FROM t GROUP BY a HAVING count(*) > $1");
    reparse_pg(&out);
}

#[test]
fn placeholder_in_join_on_condition() {
    let out = reverse("SELECT * FROM t JOIN t AS u ON t.a = u.a AND t.b = ?");
    assert_eq!(out, "SELECT * FROM t JOIN t AS u ON t.a = u.a AND t.b = $1");
    reparse_pg(&out);
}

#[test]
fn placeholder_in_order_by_expression() {
    let out = reverse("SELECT a FROM t ORDER BY a + ?");
    assert_eq!(out, "SELECT a FROM t ORDER BY a + $1");
    reparse_pg(&out);
}

#[test]
fn multi_row_insert_values_number_left_to_right() {
    let out = reverse("INSERT INTO t (a, b) VALUES (?, ?), (?, ?)");
    assert_eq!(out, "INSERT INTO t (a, b) VALUES ($1, $2), ($3, $4)");
    reparse_pg(&out);
}

#[test]
fn update_maps_set_and_where_placeholders() {
    let out = reverse("UPDATE t SET a = ?, b = ? WHERE c = ?");
    assert_eq!(out, "UPDATE t SET a = $1, b = $2 WHERE c = $3");
    reparse_pg(&out);
}

#[test]
fn delete_maps_where_placeholders_with_numbering() {
    let out = reverse("DELETE FROM t WHERE a = ? OR b = ?5");
    assert_eq!(out, "DELETE FROM t WHERE a = $1 OR b = $5");
    reparse_pg(&out);
}

#[test]
fn each_statement_numbers_placeholders_independently() {
    // Bind indices are per prepared statement, so numbering restarts at $1 for
    // each statement in a multi-statement batch.
    let out = reverse("SELECT a FROM t WHERE a = ?; SELECT b FROM t WHERE b = ?");
    assert_eq!(out, "SELECT a FROM t WHERE a = $1; SELECT b FROM t WHERE b = $1");
    for stmt in out.split("; ") {
        reparse_pg(stmt);
    }
}

#[test]
fn placeholders_in_top_level_values() {
    let out = reverse("VALUES (?, ?)");
    assert_eq!(out, "VALUES ($1, $2)");
    assert!(!out.contains('?'), "{out}");
    reparse_pg(&out);
}
