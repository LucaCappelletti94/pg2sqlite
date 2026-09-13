//! What the manifest says a column holds.
//!
//! The write half of the parameter contract is settled: a caller binds what
//! PostgreSQL takes and the emitted SQL converts. Reading back is the other
//! half, and the value a consumer reads is not always the value PostgreSQL
//! would have given it: a `NUMERIC` column holds minor units, a UUID column
//! holds sixteen bytes or canonical text depending on the representation, an
//! array column holds JSON text, and a vector column holds packed floats.
//! Each test decodes a stored value using only what the manifest publishes.

use pg2sqlite::{
    manifest::{ColumnStorage, VectorElement},
    prelude::{ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions, UuidRepresentation},
};
use rusqlite::Connection;

/// The storage of each column of `table`, in declaration order.
fn storage_of(pg: &str, options: &Pg2SqliteOptions) -> Vec<(String, ColumnStorage)> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translation_manifest(options)
        .expect("manifest")
        .into_iter()
        .flat_map(|entry| entry.columns)
        .map(|column| (column.name, column.storage))
        .collect()
}

#[test]
fn a_column_that_needs_no_decoding_says_so() {
    assert_eq!(
        storage_of(
            "CREATE TABLE t (id int, name text, b bytea, d date);",
            &Pg2SqliteOptions::default()
        ),
        vec![
            ("id".to_string(), ColumnStorage::Direct),
            ("name".to_string(), ColumnStorage::Direct),
            ("b".to_string(), ColumnStorage::Direct),
            ("d".to_string(), ColumnStorage::Direct),
        ]
    );
}

#[test]
fn a_scaled_numeric_publishes_its_scale() {
    assert_eq!(
        storage_of(
            "CREATE TABLE t (amount numeric(10,2), rate numeric(6,4));",
            &Pg2SqliteOptions::default()
        ),
        vec![
            ("amount".to_string(), ColumnStorage::MinorUnits { scale: 2 }),
            ("rate".to_string(), ColumnStorage::MinorUnits { scale: 4 }),
        ]
    );
}

#[test]
fn a_uuid_column_publishes_the_representation_in_force() {
    let pg = "CREATE TABLE t (id uuid);";
    assert_eq!(
        storage_of(
            pg,
            &Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
        ),
        vec![("id".to_string(), ColumnStorage::UuidBlob)]
    );
    assert_eq!(
        storage_of(
            pg,
            &Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text)
        ),
        vec![("id".to_string(), ColumnStorage::UuidText)]
    );
}

#[test]
fn an_array_column_says_it_holds_json() {
    assert_eq!(
        storage_of(
            "CREATE TABLE t (xs int[]);",
            &Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json)
        ),
        vec![("xs".to_string(), ColumnStorage::JsonArray)]
    );
}

#[test]
fn a_vector_column_publishes_its_element_and_width() {
    assert_eq!(
        storage_of(
            "CREATE TABLE t (id int primary key, e vector(3), h halfvec(4));",
            &Pg2SqliteOptions::default()
        ),
        vec![
            ("id".to_string(), ColumnStorage::Direct),
            (
                "e".to_string(),
                ColumnStorage::Vector { dimensions: Some(3), element: VectorElement::Float32 }
            ),
            (
                "h".to_string(),
                ColumnStorage::Vector { dimensions: Some(4), element: VectorElement::Float16 }
            ),
        ]
    );
}

#[test]
fn the_published_scale_decodes_a_value_written_through_the_emitted_sql() {
    let pg = "CREATE TABLE t (id int primary key, amount numeric(10,2));
              INSERT INTO t (id, amount) VALUES (1, 19.99);";
    let options = Pg2SqliteOptions::default();
    let statements =
        Pg2Sqlite::default().sql(pg).expect("parse").translate_to_sql(&options).expect("translate");
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection.execute_batch(&format!("{statement};")).expect("emitted statement executes");
    }
    let stored: i64 =
        connection.query_row("SELECT amount FROM t", [], |row| row.get(0)).expect("read back");

    let ColumnStorage::MinorUnits { scale } =
        storage_of(pg, &options).into_iter().find(|(name, _)| name == "amount").expect("amount").1
    else {
        panic!("a scaled column must publish its scale");
    };
    let divisor = 10_i64.pow(scale);
    assert_eq!(
        format!("{}.{:02}", stored / divisor, stored % divisor),
        "19.99",
        "stored {stored} at scale {scale}"
    );
}

#[test]
fn the_published_uuid_storage_decodes_a_value_written_through_the_emitted_sql() {
    let pg = "CREATE TABLE t (id uuid primary key);
              INSERT INTO t (id) VALUES ('550e8400-e29b-41d4-a716-446655440000');";
    let options = Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob);
    let statements =
        Pg2Sqlite::default().sql(pg).expect("parse").translate_to_sql(&options).expect("translate");
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    for statement in &statements {
        connection.execute_batch(&format!("{statement};")).expect("emitted statement executes");
    }

    assert_eq!(
        storage_of(pg, &options),
        vec![("id".to_string(), ColumnStorage::UuidBlob)],
        "the manifest has to say the column holds bytes before a reader can decode them"
    );
    let stored: Vec<u8> =
        connection.query_row("SELECT id FROM t", [], |row| row.get(0)).expect("read back");
    assert_eq!(stored.len(), 16);
    let hex = stored.iter().fold(String::new(), |mut text, byte| {
        use core::fmt::Write;
        write!(text, "{byte:02x}").expect("writing to a String cannot fail");
        text
    });
    assert_eq!(hex, "550e8400e29b41d4a716446655440000");
}
