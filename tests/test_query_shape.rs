//! The shape of a query rather than its values.
//!
//! A data-modifying CTE was emitted verbatim and cannot parse in SQLite at
//! all, `SELECT DISTINCT` accepted an `ORDER BY` PostgreSQL refuses, and
//! `LIMIT ALL` was dropped without a word. Two refusals pointed at rewrites
//! that answer something else.
//!
//! Every expected value is what PostgreSQL 17.3 answers, measured.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

const ROWS: &str = "CREATE TABLE t (a int, b int);
     INSERT INTO t VALUES (1, 10), (2, 20), (1, 30);";

/// The rows `pg` answers.
fn answer(pg: &str) -> Vec<Option<String>> {
    run_translated_with(pg, &Pg2SqliteOptions::default())
}

/// The refusal `pg` earns.
fn refusal(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("expected a refusal")
        .to_string()
}

#[test]
fn a_data_modifying_cte_is_refused() {
    // PostgreSQL answers 5. SQLite answers `near "INSERT": syntax error` when
    // the emitted statement runs, since it has no data-modifying CTE.
    let message = refusal(
        "CREATE TABLE t (a int);
         WITH x AS (INSERT INTO t VALUES (5) RETURNING a) SELECT a FROM x;",
    );
    assert!(message.contains("INSERT"), "{message}");
    assert!(message.to_lowercase().contains("common table expression"), "{message}");
}

#[test]
fn an_ordinary_cte_still_translates() {
    assert_eq!(
        answer(&format!("{ROWS} WITH x AS (SELECT a FROM t WHERE b > 15) SELECT count(*) FROM x;")),
        vec![Some("2".to_string())]
    );
}

#[test]
fn distinct_ordered_by_an_unselected_column_is_refused() {
    // PostgreSQL: ERROR: for SELECT DISTINCT, ORDER BY expressions must
    // appear in select list. The replica answered rows in an order SQLite
    // does not define, having picked one of the two b values per distinct a.
    let message = refusal(&format!("{ROWS} SELECT DISTINCT a FROM t ORDER BY b;"));
    assert!(message.contains("DISTINCT"), "{message}");
    assert!(message.to_lowercase().contains("select list"), "{message}");
}

#[test]
fn distinct_ordered_by_a_selected_column_still_works() {
    // Measured on both engines: 1, 2, and the aliased and ordinal spellings
    // of the same thing.
    assert_eq!(
        answer(&format!("{ROWS} SELECT DISTINCT a FROM t ORDER BY a;")),
        vec![Some("1".to_string()), Some("2".to_string())]
    );
    assert_eq!(
        answer(&format!("{ROWS} SELECT DISTINCT a AS k FROM t ORDER BY k;")),
        vec![Some("1".to_string()), Some("2".to_string())]
    );
    assert_eq!(
        answer(&format!("{ROWS} SELECT DISTINCT a FROM t ORDER BY 1;")),
        vec![Some("1".to_string()), Some("2".to_string())]
    );
    assert_eq!(
        answer(&format!("{ROWS} SELECT DISTINCT a + 1 FROM t ORDER BY a + 1;")),
        vec![Some("2".to_string()), Some("3".to_string())]
    );
}

#[test]
fn ordering_by_an_unselected_column_without_distinct_is_untouched() {
    // PostgreSQL allows it, so the replica has to as well.
    assert_eq!(
        answer(&format!("{ROWS} SELECT a FROM t ORDER BY b DESC;")),
        vec![Some("1".to_string()), Some("2".to_string()), Some("1".to_string())]
    );
}

#[test]
fn limit_all_answers_every_row() {
    // `LIMIT ALL` means no limit, and the parser discards the clause before
    // the translation sees it, measured as `limit_clause: None`, so there is
    // nothing to emit and nothing lost: raw SQLite answers `near "ALL":
    // syntax error` for the clause itself.
    assert_eq!(
        answer(&format!("{ROWS} SELECT a FROM t ORDER BY a LIMIT ALL;")),
        vec![Some("1".to_string()), Some("1".to_string()), Some("2".to_string())]
    );
}

#[test]
fn the_with_ties_refusal_names_the_rewrite_that_answers_the_same() {
    // Measured on {1,2,2}: FETCH FIRST 2 ROWS WITH TIES answers 1, 2, 2,
    // which RANK() OVER (ORDER BY a) <= 2 answers too, while the ROW_NUMBER()
    // the message used to suggest answers 1, 2.
    let message =
        refusal(&format!("{ROWS} SELECT a FROM t ORDER BY a FETCH FIRST 2 ROWS WITH TIES;"));
    assert!(message.contains("RANK()"), "{message}");
    assert!(
        message.contains("ROW_NUMBER() does not"),
        "the message says why the old suggestion is not it: {message}"
    );
}

#[test]
fn a_compound_select_ordered_by_an_input_column_says_so() {
    // PostgreSQL answers `column "a" does not exist` for the unqualified
    // spelling and `missing FROM-clause entry for table "t"` for the
    // qualified one: after a UNION only the output columns are in scope. The
    // old message talked about declared types and reading a column off the
    // schema.
    let message = refusal(&format!(
        "{ROWS} SELECT a + 1 AS x FROM t UNION SELECT a + 1 AS x FROM t ORDER BY a + 1;"
    ));
    assert!(message.to_lowercase().contains("union"), "{message}");
    assert!(message.to_lowercase().contains("output column"), "{message}");
}

#[test]
fn a_compound_select_ordered_by_an_output_column_still_works() {
    assert_eq!(
        answer(&format!(
            "{ROWS} SELECT a + 1 AS x FROM t UNION SELECT a + 1 AS x FROM t ORDER BY x;"
        )),
        vec![Some("2".to_string()), Some("3".to_string())]
    );
}

#[test]
fn every_writing_cte_is_refused_not_only_the_inserting_one() {
    for (body, statement) in [
        ("UPDATE t SET b = 1 RETURNING a", "UPDATE"),
        ("DELETE FROM t RETURNING a", "DELETE"),
        ("MERGE INTO t USING t s ON t.a = s.a WHEN MATCHED THEN DELETE RETURNING t.a", "MERGE"),
    ] {
        let message = refusal(&format!("{ROWS} WITH x AS ({body}) SELECT a FROM x;"));
        assert!(message.contains(statement), "{statement}: {message}");
    }
}

#[test]
fn distinct_over_a_wildcard_orders_by_any_column() {
    // The wildcard puts every column in the select list, so PostgreSQL
    // accepts the ordering and answers 1/10, 2/20, 1/30.
    assert_eq!(
        answer(&format!("{ROWS} SELECT DISTINCT * FROM t ORDER BY b;")),
        vec![Some("1".to_string()), Some("2".to_string()), Some("1".to_string())]
    );
}

#[test]
fn a_compound_select_orders_by_a_position() {
    // An ordinal names an output column, so the branches may project
    // different input columns: PostgreSQL answers 1, 2, 10, 20, 30.
    assert_eq!(
        answer(&format!("{ROWS} SELECT a FROM t UNION SELECT b FROM t ORDER BY 1;")),
        vec![
            Some("1".to_string()),
            Some("2".to_string()),
            Some("10".to_string()),
            Some("20".to_string()),
            Some("30".to_string())
        ]
    );
}

#[test]
fn a_compound_select_orders_by_the_name_a_qualified_column_carries() {
    // `t.a` names the output column `a`, and parenthesised branches carry the
    // same names: PostgreSQL answers 1, 2 for both spellings.
    for compound in [
        "SELECT t.a FROM t UNION SELECT t.a FROM t ORDER BY a",
        "(SELECT a FROM t) UNION (SELECT a FROM t) ORDER BY a",
    ] {
        assert_eq!(
            answer(&format!("{ROWS} {compound};")),
            vec![Some("1".to_string()), Some("2".to_string())],
            "{compound}"
        );
    }
}

#[test]
fn a_compound_branch_may_carry_its_own_ordering_and_limit() {
    // SQLite takes neither a parenthesised operand nor one ending in LIMIT,
    // so the branch becomes a select over a derived table. PostgreSQL answers
    // 1, 30.
    assert_eq!(
        answer(&format!(
            "{ROWS} (SELECT a FROM t ORDER BY a LIMIT 1) \
             UNION (SELECT b FROM t ORDER BY b DESC LIMIT 1);"
        )),
        vec![Some("1".to_string()), Some("30".to_string())]
    );
}
