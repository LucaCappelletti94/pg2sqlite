//! Lossy-drop warnings emitted during translation.
//!
//! `LISTEN`/`UNLISTEN`/`NOTIFY` have no SQLite equivalent, so they are
//! dropped. The new `translate_with_report` API surfaces a
//! `TranslationWarning::LossyDrop` for each so callers know what was
//! discarded.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    warnings::TranslationWarning,
};

fn warnings_for(pg: &str) -> Vec<TranslationWarning> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate")
        .warnings
}

fn assert_lossy_drop(warning: &TranslationWarning, expected_construct: &str) {
    match warning {
        TranslationWarning::LossyDrop { construct, .. } => {
            assert_eq!(*construct, expected_construct);
        }
        other => panic!("expected a LossyDrop warning, got {other:?}"),
    }
}

#[test]
fn listen_emits_lossy_drop_warning() {
    let warns = warnings_for("LISTEN ch;");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "LISTEN");
}

#[test]
fn unlisten_emits_lossy_drop_warning() {
    let warns = warnings_for("UNLISTEN ch;");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "UNLISTEN");
}

#[test]
fn notify_emits_lossy_drop_warning() {
    let warns = warnings_for("NOTIFY ch, 'msg';");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "NOTIFY");
}

#[test]
fn create_table_emits_no_warnings() {
    let warns = warnings_for("CREATE TABLE t (id INTEGER PRIMARY KEY);");
    assert!(warns.is_empty(), "expected no warnings, got {warns:?}");
}

#[test]
fn translate_with_report_preserves_translated_statements() {
    let report = Pg2Sqlite::default()
        .sql("CREATE TABLE t (id INTEGER PRIMARY KEY);")
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate");
    assert_eq!(report.statements.len(), 1);
    assert!(report.warnings.is_empty());
}

#[test]
fn mixed_statements_collect_warnings_in_order() {
    let warns = warnings_for(
        "CREATE TABLE t (id INTEGER PRIMARY KEY);\n\
         LISTEN ch;\n\
         NOTIFY ch, 'msg';",
    );
    assert_eq!(warns.len(), 2);
    assert_lossy_drop(&warns[0], "LISTEN");
    assert_lossy_drop(&warns[1], "NOTIFY");
}

#[test]
fn create_type_emits_lossy_drop_warning() {
    let warns = warnings_for("CREATE TYPE address AS (street TEXT, city TEXT);");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "CREATE TYPE");
}

#[test]
fn create_type_enum_emits_lossy_drop_warning() {
    let warns = warnings_for("CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "CREATE TYPE");
}

#[test]
fn create_domain_emits_lossy_drop_warning() {
    let warns = warnings_for("CREATE DOMAIN positive AS INTEGER CHECK (VALUE > 0);");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "CREATE DOMAIN");
}

#[test]
fn create_server_emits_lossy_drop_warning() {
    let warns = warnings_for("CREATE SERVER remote FOREIGN DATA WRAPPER postgres_fdw;");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "CREATE SERVER");
}

#[test]
fn create_role_emits_lossy_drop_warning() {
    let warns = warnings_for("CREATE ROLE alice;");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "CREATE ROLE");
}

#[test]
fn create_user_emits_lossy_drop_warning() {
    let warns = warnings_for("CREATE USER bob;");
    assert_eq!(warns.len(), 1);
    assert_lossy_drop(&warns[0], "CREATE USER");
}

#[test]
fn grant_emits_lossy_drop_warning() {
    // GRANT requires the role to exist in the schema, so seed it.
    let warns = warnings_for(
        "CREATE TABLE t (id INTEGER PRIMARY KEY);\n\
         CREATE ROLE alice;\n\
         GRANT SELECT ON t TO alice;",
    );
    assert_eq!(warns.len(), 2);
    assert_lossy_drop(&warns[0], "CREATE ROLE");
    assert_lossy_drop(&warns[1], "GRANT");
}

#[test]
fn revoke_emits_lossy_drop_warning() {
    // REVOKE requires a matching GRANT, so seed both.
    let warns = warnings_for(
        "CREATE TABLE t (id INTEGER PRIMARY KEY);\n\
         CREATE ROLE alice;\n\
         GRANT SELECT ON t TO alice;\n\
         REVOKE SELECT ON t FROM alice;",
    );
    assert_eq!(warns.len(), 3);
    assert_lossy_drop(&warns[0], "CREATE ROLE");
    assert_lossy_drop(&warns[1], "GRANT");
    assert_lossy_drop(&warns[2], "REVOKE");
}

#[test]
fn alter_role_emits_lossy_drop_warning() {
    let warns = warnings_for("CREATE ROLE alice;\nALTER ROLE alice WITH SUPERUSER;");
    assert_eq!(warns.len(), 2);
    assert_lossy_drop(&warns[0], "CREATE ROLE");
    assert_lossy_drop(&warns[1], "ALTER ROLE");
}

// ALTER USER syntax does not parse in the pinned sqlparser fork yet, so
// there is no end-to-end test. The translator arm above stays in place
// so the warning fires the moment the fork picks the construct up.
