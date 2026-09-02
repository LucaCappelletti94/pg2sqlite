//! Forward translation against a separately built schema.

use diesel::{
    prelude::*,
    sql_types::{Nullable, Text},
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;

fn schema(ddl: &str) -> ParserDB {
    Pg2Sqlite::default()
        .sql(ddl)
        .expect("schema should parse")
        .build_schema()
        .expect("schema should build")
}

fn sqlite_with(ddl: &str) -> SqliteConnection {
    let statements = Pg2Sqlite::default()
        .sql(ddl)
        .expect("schema should parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("schema should translate");
    let mut connection = SqliteConnection::establish(":memory:").expect("SQLite should connect");
    for statement in statements {
        diesel::sql_query(&statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted schema failed: {statement}: {error}"));
    }
    connection
}

#[derive(QueryableByName)]
struct TextValue {
    #[diesel(sql_type = Nullable<Text>)]
    value: Option<String>,
}

#[test]
fn external_schema_drives_column_dependent_rewrites() {
    let ddl = "CREATE TABLE docs (id INT PRIMARY KEY, payload JSONB);";
    let schema = schema(ddl);
    let report = Pg2Sqlite::default()
        .sql("SELECT json_agg(payload) AS value FROM docs;")
        .expect("query should parse")
        .translate_with_report_and_schema(&schema, &Pg2SqliteOptions::default())
        .expect("query should translate");
    assert!(report.warnings.is_empty(), "translation should be lossless");
    assert_eq!(report.statements.len(), 1);

    let mut connection = sqlite_with(ddl);
    diesel::sql_query("INSERT INTO docs VALUES (1, '{\"a\":1}'), (2, '[2,3]')")
        .execute(&mut connection)
        .expect("fixture rows should insert");
    let rows = diesel::sql_query(report.statements[0].to_string())
        .load::<TextValue>(&mut connection)
        .expect("translated query should execute");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value.as_deref(), Some(r#"[{"a":1},[2,3]]"#));
}

#[test]
fn external_schema_refuses_a_returning_from_column() {
    let schema = schema(
        "CREATE TABLE t (id INT PRIMARY KEY, v INT);
         CREATE TABLE s (id INT PRIMARY KEY, source_v INT);",
    );
    let error = Pg2Sqlite::default()
        .sql("UPDATE t SET v = s.source_v FROM s WHERE t.id = s.id RETURNING source_v;")
        .expect("update should parse")
        .translate_with_schema(&schema, &Pg2SqliteOptions::default())
        .expect_err("the source column should be refused")
        .to_string();
    assert!(error.contains("RETURNING source_v"), "unexpected error: {error}");
}

#[test]
fn loaded_ddl_updates_the_supplied_schema() {
    let ddl = "CREATE TABLE t (id INT PRIMARY KEY);";
    let schema = schema(ddl);
    let statements = Pg2Sqlite::default()
        .sql(
            "ALTER TABLE t ADD COLUMN label TEXT;
             UPDATE t SET label = 'ok' WHERE id = 1 RETURNING label AS value;",
        )
        .expect("batch should parse")
        .translate_to_sql_with_schema(&schema, &Pg2SqliteOptions::default())
        .expect("batch should translate");

    let mut connection = sqlite_with(ddl);
    diesel::sql_query("INSERT INTO t (id) VALUES (1)")
        .execute(&mut connection)
        .expect("fixture row should insert");
    diesel::sql_query(&statements[0]).execute(&mut connection).expect("alter table should execute");
    let rows = diesel::sql_query(&statements[1])
        .load::<TextValue>(&mut connection)
        .expect("update should execute");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value.as_deref(), Some("ok"));
}

#[test]
fn complete_external_schema_refuses_an_absent_returning_target() {
    let schema = schema("CREATE TABLE s (id INT PRIMARY KEY);");
    let error = Pg2Sqlite::default()
        .sql("DELETE FROM missing USING s WHERE TRUE RETURNING unknown;")
        .expect("delete should parse")
        .translate_with_schema(&schema, &Pg2SqliteOptions::default())
        .expect_err("the complete schema should reject the missing target")
        .to_string();
    assert!(error.contains("RETURNING unknown"), "unexpected error: {error}");
}
