//! A trigger function whose translated body has no statements left.
//!
//! SQLite requires at least one statement between `BEGIN` and `END`, so an
//! emptied body is `near "END": syntax error` rather than a trigger that does
//! nothing.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;

/// A function whose only statement is `RETURN NEW` carries nothing SQLite can
/// run, since a SQLite trigger has no return value.
const EMPTY_BODY_SCRIPT: &str = "CREATE TABLE t (id INT PRIMARY KEY, n INT);
     CREATE FUNCTION noop() RETURNS trigger AS $$ BEGIN RETURN NEW; END $$ LANGUAGE plpgsql;
     CREATE TRIGGER t_ai AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION noop();";

#[test]
fn a_trigger_whose_body_reduces_to_nothing_still_executes() {
    let rows = run_translated_with(
        &format!("{EMPTY_BODY_SCRIPT} INSERT INTO t VALUES (1, 10); SELECT n FROM t;"),
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("10".to_string())], "the trigger must not disturb the insert");
}

/// The trigger is still created, so a later `DROP TRIGGER` finds it and a
/// replacement of it drops the right object.
#[test]
fn the_trigger_object_still_exists() {
    let rows = run_translated_with(
        &format!(
            "{EMPTY_BODY_SCRIPT} SELECT name FROM sqlite_master WHERE type = 'trigger' ORDER BY name;"
        ),
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("t_ai".to_string())]);
}

/// An emptied body is reported, because nothing at this point can tell an
/// intentional no-op from statements the plpgsql translator dropped.
#[test]
fn an_emptied_trigger_body_warns() {
    let warnings = Pg2Sqlite::default()
        .sql(EMPTY_BODY_SCRIPT)
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate")
        .warnings;

    assert!(
        warnings.iter().any(|warning| {
            matches!(
                warning,
                pg2sqlite::warnings::TranslationWarning::LossyDrop { construct, .. }
                    if *construct == "empty trigger body"
            )
        }),
        "expected a LossyDrop naming the body, got {warnings:?}"
    );
}
