//! Materialized query columns, function values and row selection.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

diesel::table! {
    /// Function inputs.
    source (id) {
        /// A stable row identifier.
        id -> Integer,
        /// A nullable Unicode input.
        value -> Nullable<Text>,
    }
}

diesel::table! {
    /// Materialized function results.
    snapshots (id) {
        /// A stable row identifier.
        id -> Integer,
        /// A nullable character count.
        characters -> Nullable<Integer>,
    }
}

diesel::table! {
    /// Filtered source rows.
    users (id) {
        /// A stable row identifier.
        id -> Integer,
        /// A display name.
        name -> Text,
        /// Whether the row is selected.
        active -> Bool,
    }
}

diesel::table! {
    /// Materialized selected rows.
    archive (id) {
        /// A stable row identifier.
        id -> Integer,
        /// A display name.
        name -> Text,
    }
}

#[test]
fn ctas_function_output_preserves_values_and_nulls() {
    let options = Pg2SqliteOptions::default();
    let translator =
        Pg2Sqlite::default().sql("CREATE TABLE source(id INT PRIMARY KEY, value TEXT);").unwrap();
    let schema = translator.build_schema().unwrap();
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    for sql in translator.translate_to_sql(&options).unwrap() {
        connection.batch_execute(&sql).unwrap();
    }
    let rows = [
        (source::id.eq(1), source::value.eq(Some("résumé"))),
        (source::id.eq(2), source::value.eq(None::<&str>)),
    ];
    diesel::insert_into(source::table).values(&rows).execute(&mut connection).unwrap();
    let sql = "CREATE TABLE snapshots AS SELECT id, char_length(value) AS characters FROM source;";
    for sql in Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate_to_sql_with_schema(&schema, &options)
        .unwrap()
    {
        // Translated CTAS and its function are runtime syntax under test.
        connection.batch_execute(&sql).unwrap();
    }
    let actual = snapshots::table
        .select((snapshots::id, snapshots::characters))
        .order(snapshots::id.asc())
        .load::<(i32, Option<i32>)>(&mut connection)
        .unwrap();
    assert_eq!(actual, [(1, Some(6)), (2, None)]);
}

#[test]
fn ctas_materializes_selected_rows() {
    let options = Pg2SqliteOptions::default();
    let translator = Pg2Sqlite::default()
        .sql("CREATE TABLE users(id INT PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN NOT NULL);")
        .unwrap();
    let schema = translator.build_schema().unwrap();
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    for sql in translator.translate_to_sql(&options).unwrap() {
        connection.batch_execute(&sql).unwrap();
    }
    let rows = [
        (users::id.eq(1), users::name.eq("Alice"), users::active.eq(true)),
        (users::id.eq(2), users::name.eq("Bob"), users::active.eq(false)),
        (users::id.eq(3), users::name.eq("Carol"), users::active.eq(true)),
    ];
    diesel::insert_into(users::table).values(&rows).execute(&mut connection).unwrap();
    let sql = "CREATE TABLE archive AS SELECT id, name FROM users WHERE active = true;";
    for sql in Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate_to_sql_with_schema(&schema, &options)
        .unwrap()
    {
        // Translated CTAS and its predicate are runtime syntax under test.
        connection.batch_execute(&sql).unwrap();
    }
    let actual = archive::table
        .select((archive::id, archive::name))
        .order(archive::id.asc())
        .load::<(i32, String)>(&mut connection)
        .unwrap();
    assert_eq!(actual, [(1, "Alice".to_owned()), (3, "Carol".to_owned())]);
}
