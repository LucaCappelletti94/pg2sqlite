//! PostgreSQL common-table-expression bindings survive SQLite translation.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;

fn query_rows(source: &str) -> Vec<i64> {
    let script = Pg2Sqlite::default()
        .sql(source)
        .expect("parse source")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate source");
    let connection = Connection::open_in_memory().expect("open SQLite");
    for statement in script.into_vec() {
        connection
            .execute_batch(&format!("{statement};"))
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
        .prepare("SELECT id FROM result ORDER BY id")
        .expect("prepare result")
        .query_map([], |row| row.get(0))
        .expect("query result")
        .collect::<Result<_, _>>()
        .expect("read result")
}

#[test]
fn a_nonrecursive_forward_reference_reads_storage() {
    assert_eq!(
        query_rows(
            "CREATE TABLE papers (id INT PRIMARY KEY);
             INSERT INTO papers VALUES (1);
             CREATE VIEW result AS
                 WITH a AS (SELECT id FROM papers), papers AS (SELECT 42 AS id)
                 SELECT id FROM a;"
        ),
        [1]
    );
}

#[test]
fn a_nonrecursive_self_named_reference_reads_storage() {
    assert_eq!(
        query_rows(
            "CREATE TABLE papers (id INT PRIMARY KEY);
             INSERT INTO papers VALUES (2);
             CREATE VIEW result AS
                 WITH papers AS (SELECT papers.id FROM papers)
                 SELECT papers.id FROM papers;"
        ),
        [2]
    );
}

#[test]
fn a_forward_reference_can_read_an_outer_cte() {
    assert_eq!(
        query_rows(
            "CREATE VIEW result AS
                 WITH papers AS (SELECT 3 AS id)
                 SELECT id FROM (
                     WITH a AS (SELECT papers.id FROM papers),
                          papers AS (SELECT 42 AS id)
                     SELECT a.id FROM a
                 ) nested;"
        ),
        [3]
    );
}

#[test]
fn a_nonrecursive_cte_keeps_preceding_siblings_visible() {
    assert_eq!(
        query_rows(
            "CREATE TABLE papers (id INT PRIMARY KEY);
             INSERT INTO papers VALUES (1);
             CREATE VIEW result AS
                 WITH first AS (SELECT id FROM papers),
                      papers AS (SELECT 42 AS id),
                      last AS (SELECT papers.id FROM papers)
                 SELECT id FROM first UNION ALL SELECT id FROM last;"
        ),
        [1, 42]
    );
}

#[test]
fn a_recursive_forward_reference_reads_its_sibling() {
    assert_eq!(
        query_rows(
            "CREATE TABLE papers (id INT PRIMARY KEY);
             INSERT INTO papers VALUES (1);
             CREATE VIEW result AS
                 WITH RECURSIVE a AS (SELECT papers.id FROM papers),
                                papers AS (SELECT 42 AS id)
                 SELECT id FROM a;"
        ),
        [42]
    );
}

#[test]
fn quoted_cte_references_keep_aliases_and_wildcards() {
    assert_eq!(
        query_rows(
            "CREATE TABLE \"Papers\" (id INT PRIMARY KEY);
             INSERT INTO \"Papers\" VALUES (4);
             CREATE VIEW result AS
                 WITH a AS (SELECT source.* FROM \"Papers\" AS source),
                      \"Papers\" AS (SELECT 42 AS id)
                 SELECT a.* FROM a;"
        ),
        [4]
    );
}

#[test]
fn a_qualified_storage_reference_bypasses_a_same_named_cte() {
    assert_eq!(
        query_rows(
            "CREATE TABLE papers (id INT PRIMARY KEY);
             INSERT INTO papers VALUES (5);
             CREATE VIEW result AS
                 WITH papers AS (SELECT 42 AS id)
                 SELECT id FROM public.papers;"
        ),
        [5]
    );
}

#[test]
fn independently_captured_ctes_keep_existing_relation_names() {
    assert_eq!(
        query_rows(
            "CREATE TABLE papers (id INT PRIMARY KEY);
             CREATE TABLE books (id INT PRIMARY KEY);
             INSERT INTO papers VALUES (6);
             INSERT INTO books VALUES (7);
             CREATE VIEW result AS
                 WITH __pg2sqlite_cte_0 AS (SELECT 8 AS id),
                      a AS (SELECT id FROM papers),
                      papers AS (SELECT 42 AS id),
                      b AS (SELECT id FROM books),
                      books AS (SELECT 43 AS id)
                 SELECT id FROM a UNION ALL SELECT id FROM b
                 UNION ALL SELECT id FROM __pg2sqlite_cte_0
                 UNION ALL SELECT id FROM papers
                 UNION ALL SELECT id FROM books;"
        ),
        [6, 7, 8, 42, 43]
    );
}

#[test]
fn a_forward_storage_reference_follows_a_table_rename_on_reconnect() {
    let script = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE papers (id INT PRIMARY KEY);
             INSERT INTO papers VALUES (9);
             CREATE TEMP VIEW result AS
                 WITH a AS (SELECT id FROM papers), papers AS (SELECT 42 AS id)
                 SELECT id FROM a;
             ALTER TABLE papers RENAME TO archive;",
        )
        .expect("parse source")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate source");
    let directory = tempfile::tempdir().expect("database directory");
    let path = directory.path().join("main.db");
    {
        let connection = Connection::open(&path).expect("create database");
        for statement in script.iter() {
            connection.execute_batch(statement).expect("initialize");
        }
        assert_eq!(
            connection
                .query_row("SELECT id FROM result", [], |row| row.get::<_, i64>(0))
                .expect("read initial view"),
            9
        );
    }
    let connection = Connection::open(&path).expect("reopen database");
    for statement in script.connect().expect("connection program") {
        connection.execute_batch(statement).expect("reconnect");
    }
    assert_eq!(
        connection
            .query_row("SELECT id FROM result", [], |row| row.get::<_, i64>(0))
            .expect("read replayed view"),
        9
    );
}

#[test]
fn nested_forward_aliases_cannot_capture_the_same_storage_read() {
    assert_eq!(
        query_rows(
            "CREATE TABLE papers (id INT PRIMARY KEY);
             INSERT INTO papers VALUES (10);
             CREATE VIEW result AS
                 WITH a AS (
                     WITH b AS (SELECT id FROM papers), papers AS (SELECT 42 AS id)
                     SELECT id FROM b
                 ), papers AS (SELECT 43 AS id)
                 SELECT id FROM a;"
        ),
        [10]
    );
}

#[test]
fn quoted_case_distinct_ctes_do_not_capture_qualified_storage() {
    assert_eq!(
        query_rows(
            "CREATE TABLE papers (id INT PRIMARY KEY);
             INSERT INTO papers VALUES (11);
             CREATE VIEW result AS
                 WITH \"Papers\" AS (SELECT 42 AS id), papers AS (SELECT 43 AS id)
                 SELECT id FROM public.papers
                 UNION ALL SELECT id FROM \"Papers\"
                 UNION ALL SELECT id FROM papers;"
        ),
        [11, 42, 43]
    );
}
