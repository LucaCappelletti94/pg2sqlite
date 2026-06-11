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
