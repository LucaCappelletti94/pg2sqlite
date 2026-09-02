//! F17 and F32: what a RETURNING list may name once the statement reaches
//! SQLite.
//!
//! SQLite's RETURNING sees one row, the one being deleted or updated. It
//! accepts a bare column of the target, the target's real table name
//! qualifying a column, `*` meaning that row, and expressions over those. It
//! refuses every `table.*` spelling, every reference to a USING or FROM
//! relation, and every reference qualified by the target's own alias.
//! PostgreSQL accepts all five, and after `DELETE FROM t AS a` it is the
//! mirror image: `a.id` is required and `t.id` is refused.
//!
//! Two of the five have an exact SQLite spelling and are rewritten. The rest
//! are refused, including a bare `*` beside a USING clause, which PostgreSQL
//! expands over the USING relations as well and which would otherwise return
//! a silently narrower row.
//!
//! Every expectation was read off PostgreSQL 17 before the fix. The emitted
//! statements are executed as text because that text is the artifact under
//! test; everything read back afterwards goes through the typed diesel DSL.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        t3 (id) {
            id -> Integer,
            link -> Nullable<Integer>,
        }
    }

    diesel::table! {
        r3 (id) {
            id -> Integer,
            tag -> Nullable<Text>,
        }
    }
}

use schema::t3;

/// A target and a second relation to reach for from the returned list.
const FIXTURE: &str = "CREATE TABLE t3 (id INT PRIMARY KEY, link INT);
     CREATE TABLE r3 (id INT PRIMARY KEY, tag TEXT);
     INSERT INTO t3 (id, link) VALUES (1, 10), (2, 20), (3, 30), (4, 40), (5, 50);
     INSERT INTO r3 (id, tag) VALUES (10, 'x'), (20, 'y'), (30, 'z'), (40, 'w'), (50, 'v');";

/// One integer column read back out of a RETURNING clause.
#[derive(QueryableByName)]
struct Number {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
}

/// One text column read back out of a RETURNING clause.
#[derive(QueryableByName)]
struct Tag {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    tag: Option<String>,
}

fn translate(dml: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(&format!("{FIXTURE}\n{dml}"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
}

fn translate_err(dml: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{FIXTURE}\n{dml}"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("translation should be refused")
        .to_string()
}

/// Applies every emitted statement but the last, and returns the connection
/// together with the last statement, which is the one under test.
fn apply_setup(dml: &str) -> (SqliteConnection, String) {
    let mut statements = translate(dml);
    let probe = statements.pop().expect("at least one statement");
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    for statement in statements {
        diesel::sql_query(&statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted setup failed: {statement}: {error}"));
    }
    (connection, probe)
}

/// The integers a translated statement's RETURNING clause answers.
fn returned_numbers(dml: &str) -> (SqliteConnection, Vec<i32>) {
    let (mut connection, probe) = apply_setup(dml);
    let rows = diesel::sql_query(&probe)
        .load::<Number>(&mut connection)
        .unwrap_or_else(|error| panic!("emitted probe failed: {probe}: {error}"));
    let numbers = rows.into_iter().map(|row| row.id).collect();
    (connection, numbers)
}

/// The strings a translated statement's RETURNING clause answers.
fn returned_tags(dml: &str) -> Vec<Option<String>> {
    let (mut connection, probe) = apply_setup(dml);
    diesel::sql_query(&probe)
        .load::<Tag>(&mut connection)
        .unwrap_or_else(|error| panic!("emitted probe failed: {probe}: {error}"))
        .into_iter()
        .map(|row| row.tag)
        .collect()
}

/// The last emitted statement, for the two rewrites whose shape is the point.
fn emitted(dml: &str) -> String {
    translate(dml).pop().expect("at least one statement")
}

// --- refusals: the returned list reaches outside the target ---------------

#[test]
fn a_returned_using_column_is_refused() {
    let error = translate_err("DELETE FROM t3 USING r3 WHERE t3.link = r3.id RETURNING r3.tag;");
    assert!(error.contains("RETURNING"), "{error}");
    assert!(error.contains("r3"), "{error}");
}

#[test]
fn a_bare_using_column_is_refused() {
    let error = translate_err("DELETE FROM t3 USING r3 WHERE t3.link = r3.id RETURNING tag;");
    assert!(error.contains("RETURNING"), "{error}");
    assert!(error.contains("tag"), "{error}");
}

#[test]
fn a_using_column_inside_an_expression_is_refused() {
    let error = translate_err(
        "DELETE FROM t3 USING r3 WHERE t3.link = r3.id RETURNING t3.id || r3.tag AS both;",
    );
    assert!(error.contains("r3"), "{error}");
}

#[test]
fn a_using_relation_is_recognised_by_its_alias() {
    let error =
        translate_err("DELETE FROM t3 USING r3 AS rr WHERE t3.link = rr.id RETURNING rr.tag;");
    assert!(error.contains("rr"), "{error}");
}

#[test]
fn a_returned_using_star_is_refused() {
    let error = translate_err("DELETE FROM t3 USING r3 WHERE t3.link = r3.id RETURNING r3.*;");
    assert!(error.contains("r3"), "{error}");
}

/// PostgreSQL expands this over the USING relation too, four columns where
/// SQLite answers two, so passing it through returns a silently narrower row.
#[test]
fn a_star_beside_a_using_clause_is_refused() {
    let error = translate_err("DELETE FROM t3 USING r3 WHERE t3.link = r3.id RETURNING *;");
    assert!(error.contains("RETURNING *"), "{error}");
}

// --- rewrites: the returned list names the target another way -------------

/// SQLite refuses every `table.*` in a returned list, and PostgreSQL's `t3.*`
/// is exactly SQLite's `*`.
#[test]
fn the_targets_own_star_becomes_a_bare_star() {
    let sql = emitted("DELETE FROM t3 WHERE id = 2 RETURNING t3.*;");
    assert!(sql.contains("RETURNING *"), "{sql}");
    assert!(!sql.contains("t3.*"), "{sql}");

    let (mut connection, ids) = returned_numbers("DELETE FROM t3 WHERE id = 2 RETURNING t3.*;");
    assert_eq!(ids, vec![2]);
    let left = t3::table.select(t3::id).order(t3::id).load::<i32>(&mut connection).unwrap();
    assert_eq!(left, vec![1, 3, 4, 5]);
}

#[test]
fn an_alias_star_becomes_a_bare_star() {
    let (_, ids) = returned_numbers("DELETE FROM t3 AS a WHERE a.id = 3 RETURNING a.*;");
    assert_eq!(ids, vec![3]);
}

/// PostgreSQL requires the alias here and SQLite cannot resolve it, so the
/// qualifier is dropped rather than carried over.
#[test]
fn the_target_alias_qualifier_is_dropped() {
    let dml = "DELETE FROM t3 AS a USING r3 WHERE a.link = r3.id AND a.id = 1 RETURNING a.id;";
    let sql = emitted(dml);
    assert!(sql.contains("RETURNING id"), "{sql}");

    let (mut connection, ids) = returned_numbers(dml);
    assert_eq!(ids, vec![1]);
    let left = t3::table.select(t3::id).order(t3::id).load::<i32>(&mut connection).unwrap();
    assert_eq!(left, vec![2, 3, 4, 5]);
}

// --- untouched: the returned list already reads on SQLite -----------------

#[test]
fn a_returned_target_column_survives_a_using_clause() {
    let (_, ids) = returned_numbers(
        "DELETE FROM t3 USING r3 WHERE t3.link = r3.id AND t3.id = 1 RETURNING t3.id;",
    );
    assert_eq!(ids, vec![1]);
}

#[test]
fn a_bare_target_column_survives_a_using_clause() {
    let (_, links) = returned_numbers(
        "DELETE FROM t3 USING r3 WHERE t3.link = r3.id AND t3.id = 1 RETURNING link AS id;",
    );
    assert_eq!(links, vec![10]);
}

#[test]
fn a_star_without_a_using_clause_is_untouched() {
    let (_, ids) = returned_numbers("DELETE FROM t3 WHERE id = 2 RETURNING *;");
    assert_eq!(ids, vec![2]);
}

/// The relation inside a returned subquery is that subquery's own, and the
/// correlated reference back to the target resolves on SQLite.
#[test]
fn a_returned_subquery_over_another_table_is_kept() {
    let tags = returned_tags(
        "DELETE FROM t3 WHERE id = 1 RETURNING (SELECT tag FROM r3 WHERE r3.id = t3.link) AS tag;",
    );
    assert_eq!(tags, vec![Some("x".to_string())]);
}

// --- the same rules for UPDATE ... FROM, which is F32 ---------------------

#[test]
fn an_updates_returned_from_column_is_refused() {
    let error =
        translate_err("UPDATE t3 SET link = link FROM r3 WHERE t3.link = r3.id RETURNING r3.tag;");
    assert!(error.contains("RETURNING"), "{error}");
    assert!(error.contains("r3"), "{error}");
}

#[test]
fn an_updates_bare_from_column_is_refused() {
    let error =
        translate_err("UPDATE t3 SET link = link FROM r3 WHERE t3.link = r3.id RETURNING tag;");
    assert!(error.contains("tag"), "{error}");
}

#[test]
fn an_updates_star_beside_a_from_clause_is_refused() {
    let error =
        translate_err("UPDATE t3 SET link = link FROM r3 WHERE t3.link = r3.id RETURNING *;");
    assert!(error.contains("RETURNING *"), "{error}");
}

#[test]
fn an_updates_target_star_becomes_a_bare_star() {
    let dml = "UPDATE t3 SET link = link + 1 WHERE id = 4 RETURNING t3.*;";
    let sql = emitted(dml);
    assert!(sql.contains("RETURNING *"), "{sql}");

    let (mut connection, ids) = returned_numbers(dml);
    assert_eq!(ids, vec![4]);
    let link = t3::table.find(4).select(t3::link).first::<Option<i32>>(&mut connection).unwrap();
    assert_eq!(link, Some(41));
}

#[test]
fn an_updates_target_alias_qualifier_is_dropped() {
    let (_, ids) =
        returned_numbers("UPDATE t3 AS a SET link = link + 1 WHERE a.id = 4 RETURNING a.id;");
    assert_eq!(ids, vec![4]);
}

#[test]
fn an_updates_returned_target_column_survives_a_from_clause() {
    let (_, ids) = returned_numbers(
        "UPDATE t3 SET link = link FROM r3 WHERE t3.link = r3.id AND t3.id = 5 RETURNING t3.id;",
    );
    assert_eq!(ids, vec![5]);
}

// --- the evidence the bare-name rule leans on -----------------------------

/// The target's columns are unknown here, so the refusal falls back to asking
/// whether the name is a column of a declared USING relation.
#[test]
fn an_undeclared_target_refuses_a_bare_column_of_the_using_relation() {
    let error = translate_err("DELETE FROM nope USING r3 WHERE TRUE RETURNING tag;");
    assert!(error.contains("tag"), "{error}");
}

/// Neither side resolves the name, so nothing is claimed about it and SQLite
/// reports it, exactly as it does today.
#[test]
fn a_name_no_relation_declares_is_left_to_sqlite() {
    let sql = emitted("DELETE FROM nope USING r3 WHERE TRUE RETURNING whatever;");
    assert!(sql.contains("RETURNING whatever"), "{sql}");
}
