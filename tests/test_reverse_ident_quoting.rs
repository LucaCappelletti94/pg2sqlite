//! Reverse translation must normalize SQLite delimited-identifier quoting to
//! PostgreSQL double quotes. SQLite accepts `` `ident` ``, `[ident]`, and
//! `"ident"`, but PostgreSQL accepts only `"ident"`, so reverse output that
//! keeps backtick or bracket quoting is not the PostgreSQL it claims to be.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

const SCHEMA: &str = "CREATE TABLE t (c INT, d INT); CREATE TABLE t2 (c INT);";

fn reverse(sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(SCHEMA).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();
    stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
}

#[test]
fn reverse_translation_rewrites_backtick_idents() {
    let out = reverse("SELECT `t`.`c` FROM `t` WHERE `t`.`c` > 1 ORDER BY `t`.`c`");
    assert_eq!(out, r#"SELECT "t"."c" FROM "t" WHERE "t"."c" > 1 ORDER BY "t"."c""#);
}

#[test]
fn reverse_translation_rewrites_bracket_idents() {
    let out = reverse("SELECT [t].[c] FROM [t] WHERE [t].[c] > 1 ORDER BY [t].[c]");
    assert_eq!(out, r#"SELECT "t"."c" FROM "t" WHERE "t"."c" > 1 ORDER BY "t"."c""#);
}

#[test]
fn reverse_translation_preserves_ident_text() {
    // Backtick-quoted identifier whose text carries a double quote. The reverse
    // output must escape it the PostgreSQL way ("") and reparse cleanly, with
    // the identifier text preserved byte for byte.
    let out = reverse(r#"SELECT `a"b` FROM t"#);
    assert!(out.contains(r#""a""b""#), "expected escaped double-quoted ident: {out}");
    assert!(!out.contains('`'), "{out}");
    Parser::parse_sql(&PostgreSqlDialect {}, &out)
        .unwrap_or_else(|e| panic!("output must reparse under PostgreSqlDialect: {e}\n{out}"));
}

#[test]
fn reverse_translation_leaves_pg_quoting_alone() {
    assert_eq!(reverse("SELECT c FROM t"), "SELECT c FROM t");
    assert_eq!(reverse(r#"SELECT "t"."c" FROM "t""#), r#"SELECT "t"."c" FROM "t""#);
}

#[test]
fn reverse_translation_rewrites_backtick_idents_in_insert() {
    let out = reverse("INSERT INTO `t` (`c`) VALUES (1)");
    assert_eq!(out, r#"INSERT INTO "t" ("c") VALUES (1)"#);
    assert!(!out.contains('`'), "{out}");
}

#[test]
fn reverse_translation_rewrites_backtick_idents_in_update() {
    let out = reverse("UPDATE `t` SET `c` = 2 WHERE `c` = 1");
    assert_eq!(out, r#"UPDATE "t" SET "c" = 2 WHERE "c" = 1"#);
    assert!(!out.contains('`'), "{out}");
}

#[test]
fn reverse_translation_rewrites_backtick_idents_in_delete() {
    let out = reverse("DELETE FROM `t` WHERE `c` = 1");
    assert_eq!(out, r#"DELETE FROM "t" WHERE "c" = 1"#);
    assert!(!out.contains('`'), "{out}");
}

fn reparse_pg(sql: &str) {
    Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap_or_else(|e| {
        panic!("reverse output must reparse under PostgreSqlDialect: {e}\n{sql}")
    });
}

#[test]
fn reverse_translation_rewrites_aliases() {
    let out = reverse("SELECT `c` AS `alias` FROM `t` AS `x`");
    assert_eq!(out, r#"SELECT "c" AS "alias" FROM "t" AS "x""#);
}

#[test]
fn reverse_translation_rewrites_join_and_subquery() {
    let out = reverse(
        "SELECT `x`.`c` FROM `t` AS `x` JOIN `t` AS `y` ON `x`.`c` = `y`.`c` WHERE `x`.`c` IN (SELECT `c` FROM `t`)",
    );
    assert!(!out.contains('`'), "{out}");
    assert!(out.contains(r#""x"."c""#) && out.contains(r#""y"."c""#), "{out}");
    reparse_pg(&out);
}

#[test]
fn reverse_translation_rewrites_cte_names() {
    let out = reverse("WITH `cte` AS (SELECT `c` FROM `t`) SELECT `c` FROM `cte`");
    assert!(!out.contains('`'), "{out}");
    assert!(out.contains(r#""cte""#), "{out}");
    reparse_pg(&out);
}

#[test]
fn reverse_translation_rewrites_function_name() {
    let out = reverse("SELECT `max`(`c`) FROM `t`");
    assert!(!out.contains('`'), "{out}");
    assert!(out.contains(r#""max"("c")"#), "{out}");
    reparse_pg(&out);
}

#[test]
fn reverse_translation_rewrites_returning() {
    let out = reverse("INSERT INTO `t` (`c`) VALUES (1) RETURNING `c`");
    assert_eq!(out, r#"INSERT INTO "t" ("c") VALUES (1) RETURNING "c""#);
    assert!(!out.contains('`'), "{out}");
}

#[test]
fn reverse_output_reparses_as_postgres() {
    // Property: any statement the reverse translation accepts renders as valid
    // PostgreSQL, across projection, FROM, aliases, joins, subqueries, CTEs,
    // and DML, in both backtick and bracket quoting.
    let corpus = [
        "SELECT `t`.`c` FROM `t` WHERE `t`.`c` > 1 ORDER BY `t`.`c`",
        "SELECT [t].[c] FROM [t]",
        "SELECT `t`.[c] FROM [t]",
        "SELECT `t`.* FROM `t`",
        "SELECT `c` AS `alias` FROM `t` AS `x`",
        "SELECT DISTINCT `c` FROM `t`",
        "SELECT `c` FROM `t` GROUP BY `c` HAVING `c` > 0",
        "SELECT ROW_NUMBER() OVER (PARTITION BY `c` ORDER BY `c`) FROM `t`",
        "SELECT COUNT(*) FILTER (WHERE `c` > 0) FROM `t`",
        "SELECT `s`.`c` FROM (SELECT `c` FROM `t`) AS `s`",
        "SELECT CASE WHEN `c` > 0 THEN `c` ELSE `d` END FROM `t`",
        "SELECT `c` FROM `t` WHERE `c` IN (1, 2) AND `c` BETWEEN 1 AND 2",
        "SELECT `c` FROM `t` JOIN `t2` USING (`c`)",
        "SELECT `x`.`c` FROM `t` AS `x` JOIN `t` AS `y` ON `x`.`c` = `y`.`c`",
        "WITH `cte` AS (SELECT `c` FROM `t`) SELECT `c` FROM `cte`",
        "WITH `cte` (`a`) AS (SELECT `c` FROM `t`) SELECT `a` FROM `cte`",
        "SELECT ROW_NUMBER() OVER `w` FROM `t` WINDOW `w` AS (PARTITION BY `c`)",
        "INSERT INTO `t` (`c`, `d`) VALUES (1, 2) RETURNING `c`",
        "UPDATE [t] SET [c] = 2 WHERE [c] = 1",
        "DELETE FROM `t` WHERE `c` = 1",
    ];
    for sql in corpus {
        let out = reverse(sql);
        assert!(!out.contains('`') && !out.contains('['), "quoting leaked for {sql}: {out}");
        reparse_pg(&out);
    }
}
