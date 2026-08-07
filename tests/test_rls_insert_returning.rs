//! Red tests: `INSERT INTO <view> ... RETURNING ...` against an
//! RLS-translated table must surface the row the INSTEAD OF INSERT
//! trigger writes into the backing table. Today RETURNING through
//! the view yields NULL columns because the trigger forwards the
//! INSERT to the backing table while RETURNING reads from the view.
//!
//! These tests pin the expected behaviour: every column listed in
//! RETURNING (including auto-assigned integer primary keys) must
//! come back as the value actually stored.

use pg2sqlite::prelude::{
    Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, TranslationOptions, UuidRepresentation,
};
use rusqlite::Connection;

const RLS_SCHEMA: &str = "\
CREATE TABLE documents (
    id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    title TEXT NOT NULL,
    content TEXT
);
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
CREATE POLICY documents_select_policy ON documents
    FOR SELECT USING (owner_id = current_setting('app.user_id')::integer);
CREATE POLICY documents_insert_policy ON documents
    FOR INSERT WITH CHECK (owner_id = current_setting('app.user_id')::integer);
INSERT INTO documents (id, owner_id, title) VALUES (1, 42, 'First');
";

fn opts() -> Pg2SqliteOptions {
    // Strict RLS validation is the gate that unlocks the INSERT-RETURNING
    // rewrite + the backing-table BEFORE INSERT guard trigger. Default
    // monitor mode keeps the existing INSTEAD OF INSERT path (which
    // surfaces NULL for RETURNING) so monitor-mode audit scenarios are
    // not broken by this change.
    Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_rls_audit_table_name("rls_violations")
        .with_strict_rls_validation()
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
}

fn translate(schema: &str, opts: &Pg2SqliteOptions) -> String {
    Pg2Sqlite::default()
        .sql(schema)
        .expect("parse")
        .translate(opts)
        .expect("translate")
        .iter()
        .map(|s| format!("{s};"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn translate_query_tail(pg_query: &str, schema: &str, opts: &Pg2SqliteOptions) -> String {
    // Same shape as `translator::translate_query` in the playground:
    // translate `schema + query` together and keep only the tail.
    let schema_stmts =
        Pg2Sqlite::default().sql(schema).expect("parse").translate(opts).expect("translate");
    let combined = format!("{schema};\n{pg_query}");
    let combined_stmts =
        Pg2Sqlite::default().sql(&combined).expect("parse").translate(opts).expect("translate");
    combined_stmts
        .into_iter()
        .skip(schema_stmts.len())
        .map(|s| format!("{s};"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn open_with_session_user() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.create_scalar_function(
        "current_app_user",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |_| Ok(42i64),
    )
    .unwrap();
    conn
}

#[test]
fn insert_returning_id_yields_the_assigned_primary_key() {
    let opts = opts();
    let conn = open_with_session_user();
    conn.execute_batch(&translate(RLS_SCHEMA, &opts)).expect("apply schema");

    let sql = translate_query_tail(
        "INSERT INTO documents (owner_id, title) VALUES (42, 'r') RETURNING id",
        RLS_SCHEMA,
        &opts,
    );
    let returned: Option<i64> = conn
        .query_row(sql.trim_end_matches(";\n").trim_end_matches(';'), [], |r| r.get(0))
        .expect("RETURNING id must produce a row");
    let id = returned
        .expect("RETURNING id must surface the auto-assigned PK from the backing table, got NULL");
    assert!(id > 0, "the returned PK should be a positive integer, got {id}");
}

#[test]
fn insert_returning_when_policy_fails_raises() {
    // Companion negative test: an INSERT-with-RETURNING that violates
    // the WITH CHECK policy must still be rejected. Proves the
    // translate-time rewrite to the backing table did not silently
    // bypass policy enforcement (the backing-table BEFORE INSERT
    // guard trigger picks up the slack).
    let opts = opts();
    let conn = open_with_session_user();
    conn.execute_batch(&translate(RLS_SCHEMA, &opts)).expect("apply schema");

    let sql = translate_query_tail(
        "INSERT INTO documents (owner_id, title) VALUES (99, 't') RETURNING id",
        RLS_SCHEMA,
        &opts,
    );
    let err = conn
        .query_row(sql.trim_end_matches(";\n").trim_end_matches(';'), [], |r| r.get::<_, i64>(0))
        .expect_err("policy must reject RETURNING-bearing INSERT with foreign owner_id");
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("row-level security policy") || err_msg.contains("RLS"),
        "expected RLS abort message, got: {err_msg}"
    );
}

#[test]
fn plain_insert_still_uses_view_path() {
    // Symmetric positive test: a plain INSERT (no RETURNING) must NOT be
    // rewritten. The translated INSERT must continue to target the view
    // so the existing INSTEAD OF view trigger (deny-by-default + WITH
    // CHECK) keeps owning enforcement.
    let opts = opts();
    let translated = Pg2Sqlite::default()
        .sql(&format!(
            "{RLS_SCHEMA};\nINSERT INTO documents (owner_id, title) VALUES (42, 'plain');"
        ))
        .expect("parse")
        .translate(&opts)
        .expect("translate");
    let last = translated.last().expect("at least one stmt").to_string();
    assert!(
        last.contains("INSERT INTO documents (") && !last.contains("documents_rls"),
        "plain INSERT must target the view, got: {last}"
    );
    let conn = open_with_session_user();
    conn.execute_batch(&translate(RLS_SCHEMA, &opts)).expect("apply schema");
    conn.execute_batch(&format!("{last};")).unwrap();
}

#[test]
fn insert_returning_through_custom_rls_suffix() {
    // The rewrite must honour `with_rls_table_suffix(...)`, not the
    // hardcoded `_rls` default. Confirm by translating with a custom
    // suffix and asserting the rewritten INSERT targets the configured
    // backing-table name.
    let custom_opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_rls_audit_table_name("rls_violations")
        .with_strict_rls_validation()
        .with_rls_table_suffix("_inner")
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));
    let translated = Pg2Sqlite::default()
        .sql(&format!(
            "{RLS_SCHEMA};\nINSERT INTO documents (owner_id, title) VALUES (42, 'x') RETURNING id;"
        ))
        .expect("parse")
        .translate(&custom_opts)
        .expect("translate");
    let last = translated.last().expect("at least one stmt").to_string();
    assert!(
        last.contains("INSERT INTO documents_inner"),
        "rewrite must use the configured suffix, got: {last}"
    );
    let conn = open_with_session_user();
    conn.execute_batch(&translate(RLS_SCHEMA, &custom_opts)).expect("apply schema");
    conn.execute_batch(&format!("{last};")).unwrap();
}

#[test]
fn insert_returning_star_yields_full_row_from_backing_table() {
    let opts = opts();
    let conn = open_with_session_user();
    conn.execute_batch(&translate(RLS_SCHEMA, &opts)).expect("apply schema");

    let sql = translate_query_tail(
        "INSERT INTO documents (owner_id, title) VALUES (42, 'star') RETURNING *",
        RLS_SCHEMA,
        &opts,
    );
    let (id, owner_id, title): (Option<i64>, Option<i64>, Option<String>) = conn
        .query_row(sql.trim_end_matches(";\n").trim_end_matches(';'), [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .expect("RETURNING * must produce a row");

    assert!(id.is_some(), "RETURNING * must surface the assigned PK, got NULL");
    assert_eq!(owner_id, Some(42), "owner_id must come back as written");
    assert_eq!(title.as_deref(), Some("star"), "title must come back as written");
}
