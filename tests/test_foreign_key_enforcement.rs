//! Whether the emitted schema actually enforces the foreign keys it declares.
//!
//! SQLite enforces a foreign key only when the connection carries `PRAGMA
//! foreign_keys = ON`, which is off by default and is connection state rather
//! than database state. A replica applied on a default connection therefore
//! declared every foreign key and enforced none of them, silently: the delete
//! of a referenced parent row that PostgreSQL refuses left an orphan behind.
//!
//! The emitted script configures the connection it is applied to, which is
//! what the `case_sensitive_like` pragma already does, and the warning says
//! the part a script cannot do: every other connection that writes needs the
//! pragma too.

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    warnings::TranslationWarning,
};
use rusqlite::Connection;

const PARENT_AND_CHILD: &str = "CREATE TABLE p (id int primary key);
     CREATE TABLE c (id int primary key, pid int REFERENCES p(id));
     INSERT INTO p VALUES (1);
     INSERT INTO c VALUES (10, 1);";

/// Applies every emitted statement on a connection that starts with foreign
/// keys off, which is SQLite's own default, and answers the error of the
/// first statement that fails, or `None`.
///
/// The pragma is turned off explicitly because `rusqlite` turns it on for
/// every connection it opens, measured as `PRAGMA foreign_keys = 1`, where
/// the `sqlite3` command line and a plain C connection answer 0. Leaving
/// that alone would make these tests pass on a driver default rather than on
/// what the emitted script says.
fn apply_on_a_default_connection(pg: &str) -> Option<String> {
    let statements = Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("fixture translates");
    let connection = Connection::open_in_memory().expect("in-memory SQLite");
    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .expect("a connection as SQLite hands it out");
    for statement in &statements {
        if let Err(error) = connection.execute_batch(&format!("{statement};")) {
            return Some(error.to_string());
        }
    }
    None
}

/// The statements `pg` emits.
fn emitted(pg: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("fixture translates")
}

/// The warnings `pg` emits.
fn warnings(pg: &str) -> Vec<TranslationWarning> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("fixture translates")
        .warnings
}

#[test]
fn a_declared_foreign_key_is_enforced_on_a_default_connection() {
    // PostgreSQL: `update or delete on table "p" violates foreign key
    // constraint`. Without the pragma SQLite deleted the parent and left the
    // orphan.
    let failure = apply_on_a_default_connection(&format!(
        "{PARENT_AND_CHILD}
         DELETE FROM p WHERE id = 1;"
    ))
    .expect("the delete must fail as it does on the server");
    assert!(failure.to_uppercase().contains("FOREIGN KEY"), "{failure}");
}

#[test]
fn an_orphan_insert_is_refused_on_a_default_connection() {
    let failure = apply_on_a_default_connection(
        "CREATE TABLE p (id int primary key);
         CREATE TABLE c (id int primary key, pid int REFERENCES p(id));
         INSERT INTO c VALUES (10, 99);",
    )
    .expect("the insert must fail as it does on the server");
    assert!(failure.to_uppercase().contains("FOREIGN KEY"), "{failure}");
}

#[test]
fn the_pragma_is_emitted_only_where_a_foreign_key_is_declared() {
    let with_key = emitted(PARENT_AND_CHILD);
    assert!(
        with_key[0].to_lowercase().contains("pragma foreign_keys"),
        "the pragma leads the script: {with_key:?}"
    );
    let without_key = emitted("CREATE TABLE p (id int primary key);");
    assert!(
        !without_key.iter().any(|s| s.to_lowercase().contains("foreign_keys")),
        "a schema with no foreign key needs no pragma: {without_key:?}"
    );
}

#[test]
fn a_foreign_key_added_by_alter_table_is_covered_too() {
    // The pragma follows the emitted statements, not the input's shape, so a
    // key that arrives through any path is covered.
    let statements = emitted(
        "CREATE TABLE p (id int primary key);
         CREATE TABLE c (id int primary key, pid int);
         CREATE TABLE d (id int primary key, pid int REFERENCES p(id));",
    );
    assert!(statements[0].to_lowercase().contains("pragma foreign_keys"), "{statements:?}");
}

#[test]
fn the_connection_requirement_is_reported() {
    // A script can set the pragma on the connection it runs on and on no
    // other, so the caller has to be told.
    let reported = warnings(PARENT_AND_CHILD);
    let named: Vec<String> = reported
        .iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LossyDowngrade { construct, reason, .. }
                    if construct == "FOREIGN KEY" =>
                {
                    Some(reason.clone())
                }
                _ => None,
            }
        })
        .collect();
    assert_eq!(named.len(), 1, "{reported:?}");
    assert!(named[0].contains("foreign_keys"), "{}", named[0]);
    assert!(named[0].contains("connection"), "{}", named[0]);
}

#[test]
fn a_schema_with_no_foreign_key_reports_nothing_about_the_pragma() {
    let reported = warnings("CREATE TABLE p (id int primary key, v text);");
    assert!(!reported.iter().any(|w| format!("{w:?}").contains("foreign_keys")), "{reported:?}");
}
