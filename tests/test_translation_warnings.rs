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
    assert!(report.warnings.is_empty(), "nothing was dropped, got {:?}", report.warnings);
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

#[test]
fn alter_user_emits_lossy_drop_warning() {
    // Postgres `ALTER USER` is a synonym for `ALTER ROLE` and parses as
    // `Statement::AlterRole` on apache main (upstream #2374), so it
    // collapses onto the ALTER ROLE warning.
    let warns = warnings_for("CREATE USER alice;\nALTER USER alice WITH SUPERUSER;");
    assert_eq!(warns.len(), 2);
    assert_lossy_drop(&warns[0], "CREATE USER");
    assert_lossy_drop(&warns[1], "ALTER ROLE");
}

/// The downgrades a column type suffers, which used to be silent.
fn downgrades_for(pg: &str) -> Vec<(String, String, String)> {
    warnings_for(pg)
        .into_iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LossyDowngrade { location, from, to, .. } => {
                    Some((location, from, to))
                }
                _ => None,
            }
        })
        .collect()
}

/// SQLite has no zone-aware temporal type, so the column becomes TEXT holding
/// whatever offset the writer put in it. Every PostgreSQL spelling that
/// carries a zone loses it, and all four used to say nothing.
#[test]
fn a_zone_carrying_column_reports_the_zone_it_loses() {
    for declared in ["TIMESTAMPTZ", "TIMESTAMP WITH TIME ZONE", "TIMETZ", "TIME WITH TIME ZONE"] {
        let downgrades =
            downgrades_for(&format!("CREATE TABLE events (id INT PRIMARY KEY, at {declared});"));
        assert_eq!(downgrades.len(), 1, "{declared} should report one downgrade: {downgrades:?}");
        assert_eq!(downgrades[0].0, "events.at", "the warning should name the table and column");
        assert_eq!(downgrades[0].2, "TEXT");
    }
}

/// Guards the report from firing on a temporal type with no zone to lose.
/// This passes before the change too.
#[test]
fn a_zoneless_temporal_column_reports_nothing() {
    for declared in ["TIMESTAMP", "TIMESTAMP WITHOUT TIME ZONE", "TIME", "DATE"] {
        let downgrades =
            downgrades_for(&format!("CREATE TABLE events (id INT PRIMARY KEY, at {declared});"));
        assert!(downgrades.is_empty(), "{declared} loses no zone: {downgrades:?}");
    }
}

/// A column name alone does not identify a column, so the location carries the
/// table too.
#[test]
fn a_column_downgrade_names_its_table() {
    let downgrades = downgrades_for("CREATE TABLE people (id INT PRIMARY KEY, initials CHAR(3));");
    assert_eq!(downgrades.len(), 1, "{downgrades:?}");
    assert_eq!(downgrades[0].0, "people.initials");
}

/// A column added later knows its table just as well.
#[test]
fn an_added_column_downgrade_names_its_table() {
    let downgrades = downgrades_for(
        "CREATE TABLE people (id INT PRIMARY KEY);
         ALTER TABLE people ADD COLUMN seen_at TIMESTAMPTZ;",
    );
    assert_eq!(downgrades.len(), 1, "{downgrades:?}");
    assert_eq!(downgrades[0].0, "people.seen_at");
}

/// The operator class disappears while the index is still emitted, so the
/// report has to say which column lost which class rather than that some
/// index somewhere did.
#[test]
fn an_operator_class_downgrade_names_the_column_and_the_class() {
    let downgrades = downgrades_for(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
         CREATE INDEX i ON t (s text_pattern_ops);",
    );
    assert_eq!(downgrades.len(), 1, "{downgrades:?}");
    assert_eq!(downgrades[0].0, "s", "the location should name the indexed column");
    assert!(
        downgrades[0].1.contains("text_pattern_ops"),
        "the warning should name the class that was dropped: {downgrades:?}"
    );
}
