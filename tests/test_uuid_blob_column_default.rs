//! A UUID column's text-literal `DEFAULT` under Blob representation must fire
//! as sixteen bytes, not as the text string that a BLOB STRICT column refuses.
//!
//! PostgreSQL coerces a text-literal UUID default to the column's type at
//! `CREATE TABLE` time, so the stored default is already sixteen bytes on the
//! server. Under `UuidRepresentation::Blob` the translator must emit the same
//! binary form, or any insert that omits the column answers "cannot store TEXT
//! value in BLOB column".
//!
//! The `INSTEAD OF INSERT` trigger the RLS path builds reads the same default
//! expression from the same accessor the column definition does, so the fix has
//! to reach both consumers.

mod helpers;

use diesel::prelude::*;
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, TranslationOptions, UuidRepresentation};
use rosetta_uuid::Uuid;

const LITERAL: &str = "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11";

/// The sixteen bytes a parse of `LITERAL` gives, which is what the stored
/// default must equal under the Blob representation.
fn expected_bytes() -> Vec<u8> {
    <[u8; 16]>::from(LITERAL.parse::<Uuid>().expect("valid UUID literal")).to_vec()
}

fn blob_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Blob)
}

fn rls_options() -> Pg2SqliteOptions {
    blob_options().with_rls_audit_table_name("rls_audit")
}

fn text_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_uuid_representation(UuidRepresentation::Text)
}

/// Tables for the Blob representation tests.
mod blob_tables {
    diesel::table! {
        /// The view (or plain table) callers write through.
        t (id) {
            id -> Binary,
            owner -> Text,
        }
    }

    diesel::table! {
        /// The RLS backing table, read directly to verify what was stored.
        t_rls (id) {
            id -> Binary,
            owner -> Text,
        }
    }
}

/// Tables for the Text representation test.
mod text_tables {
    diesel::table! {
        t (id) {
            id -> Text,
            owner -> Text,
        }
    }
}

/// Translates `pg` and applies the emitted DDL, which is the artifact under
/// test. Returns the connection so callers can exercise the schema.
fn apply(pg: &str, options: &Pg2SqliteOptions) -> SqliteConnection {
    let translated =
        Pg2Sqlite::default().sql(pg).expect("parse").translate(options).expect("translate");
    let mut conn = establish_connection();
    for statement in &translated {
        // DDL cannot be expressed via the typed DSL; every other statement is.
        diesel::sql_query(statement.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|err| panic!("emitted DDL failed: {err}\n{statement}"));
    }
    conn
}

const TABLE_BLOB: &str = "
    CREATE TABLE t (
        id   UUID PRIMARY KEY DEFAULT 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11',
        owner TEXT NOT NULL
    );
";

const TABLE_RLS: &str = "
    CREATE TABLE t (
        id   UUID PRIMARY KEY DEFAULT 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11',
        owner TEXT NOT NULL
    );
    ALTER TABLE t ENABLE ROW LEVEL SECURITY;
    CREATE POLICY p ON t USING (owner = 'alice');
";

const TABLE_TEXT: &str = "
    CREATE TABLE t (
        id   UUID PRIMARY KEY DEFAULT 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11',
        owner TEXT NOT NULL
    );
";

/// An insert omitting the UUID column must store sixteen bytes equal to the
/// literal's binary form, not a TEXT value that BLOB STRICT refuses.
#[test]
fn blob_default_fires_as_bytes_on_plain_table() {
    use blob_tables::t;
    let mut conn = apply(TABLE_BLOB, &blob_options());

    diesel::insert_into(t::table)
        .values(t::owner.eq("alice"))
        .execute(&mut conn)
        .expect("an insert omitting the UUID column must succeed when the default is binary");

    let stored: Vec<u8> = t::table.select(t::id).first(&mut conn).expect("row");

    assert_eq!(
        stored,
        expected_bytes(),
        "stored UUID must be the 16-byte binary form of the literal"
    );
}

/// The same default must appear in the INSTEAD OF INSERT trigger the RLS path
/// builds, so a write through the view also stores the correct bytes.
#[test]
fn blob_default_fires_as_bytes_through_rls_view() {
    use blob_tables::{t, t_rls};
    let mut conn = apply(TABLE_RLS, &rls_options());

    diesel::insert_into(t::table)
        .values(t::owner.eq("alice"))
        .execute(&mut conn)
        .expect("an insert through the view omitting the UUID column must succeed");

    let stored: Vec<u8> = t_rls::table.select(t_rls::id).first(&mut conn).expect("row");

    assert_eq!(
        stored,
        expected_bytes(),
        "stored UUID through the RLS view must match the 16-byte binary form of the literal"
    );
}

/// A text literal that is not a valid UUID cannot be converted to sixteen
/// bytes. PostgreSQL refuses this at CREATE TABLE time, and the translator must
/// too, naming the column and the literal.
#[test]
fn malformed_uuid_literal_default_is_refused() {
    let pg = "CREATE TABLE t (
        id    UUID PRIMARY KEY DEFAULT 'not-a-uuid',
        owner TEXT NOT NULL
    );";

    let error = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(&blob_options())
        .expect_err("a malformed UUID literal default must be refused")
        .to_string();

    assert!(error.contains("id"), "the error must name the column, got: {error}");
    assert!(error.contains("not-a-uuid"), "the error must name the literal, got: {error}");
}

/// Under Text representation the default passes through unchanged, and an
/// insert omitting the column stores the hyphenated string.
#[test]
fn text_representation_preserves_default() {
    use text_tables::t;
    let mut conn = apply(TABLE_TEXT, &text_options());

    diesel::insert_into(t::table)
        .values(t::owner.eq("alice"))
        .execute(&mut conn)
        .expect("insert omitting the UUID column under Text representation");

    let stored: String = t::table.select(t::id).first(&mut conn).expect("row");

    assert_eq!(
        stored, LITERAL,
        "under Text representation the default must be the literal string unchanged"
    );
}
