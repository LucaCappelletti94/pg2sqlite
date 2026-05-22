//! End-to-end check that the playground's RLS sample translates and then
//! applies cleanly against an in-memory SQLite when a no-arg
//! `current_app_user()` UDF is registered returning `42` (matching how
//! the playground's `db::reopen` configures the connection). This pins
//! the user-visible behaviour so a regression in either the sample SQL
//! or the translator's trigger emission shows up as a normal test
//! failure rather than a "in-memory apply failed" toast.

use pg2sqlite::prelude::{
    Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, TranslationOptions, UuidRepresentation,
};
use rusqlite::{Connection, params};

const RLS_SAMPLE: &str = "\
CREATE TABLE documents (
    id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    title TEXT NOT NULL,
    content TEXT
);

ALTER TABLE documents ENABLE ROW LEVEL SECURITY;

CREATE POLICY documents_select_policy ON documents
    FOR SELECT
    USING (owner_id = current_setting('app.user_id')::integer);

CREATE POLICY documents_insert_policy ON documents
    FOR INSERT
    WITH CHECK (owner_id = current_setting('app.user_id')::integer);

INSERT INTO documents (id, owner_id, title) VALUES
    (1, 42, 'First draft'),
    (2, 42, 'Second draft');
";

fn translate_rls_sample() -> String {
    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_rls_audit_table_name("rls_violations")
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));
    let stmts =
        Pg2Sqlite::default().sql(RLS_SAMPLE).expect("parse").translate(&opts).expect("translate");
    stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n")
}

#[test]
fn rls_sample_applies_to_in_memory_sqlite() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.create_scalar_function(
        "current_app_user",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |_| Ok(42i64),
    )
    .expect("register current_app_user");

    let sql = translate_rls_sample();
    conn.execute_batch(&sql).expect("apply translated RLS sample");

    let visible_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
        .expect("count documents view");
    assert_eq!(visible_rows, 2, "user 42 should see exactly the two seeded rows");

    let backing_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM documents_rls", [], |row| row.get(0))
        .expect("count documents_rls backing table");
    assert_eq!(backing_rows, 2, "backing table should hold the two seeded rows");
}

#[test]
fn rls_sample_blocks_disallowed_insert() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.create_scalar_function(
        "current_app_user",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |_| Ok(42i64),
    )
    .expect("register current_app_user");

    let sql = translate_rls_sample();
    conn.execute_batch(&sql).expect("apply translated RLS sample");

    let result = conn.execute(
        "INSERT INTO documents (id, owner_id, title) VALUES (?1, ?2, ?3)",
        params![99i64, 99i64, "Owned by someone else"],
    );
    let err = result.expect_err("policy must reject foreign-owner insert");
    assert!(
        err.to_string().contains("new row violates row-level security policy"),
        "unexpected error: {err}"
    );
}
