//! `MERGE` must be refused, not discarded.
//!
//! `MERGE` performs conditional inserts, updates, and deletes in one statement,
//! so dropping it silently loses writes. It used to sit in the
//! `unsupported_statement_patterns!()` catch-all.
//!
//! `INSERT ... ON CONFLICT DO UPDATE` looks like a translation and is not one.
//! Three reasons, each measured rather than assumed, the first two against
//! PostgreSQL 16 and SQLite directly:
//!
//! A `MERGE` `ON` clause is an ordinary join predicate, while an upsert's
//! conflict target must name a PRIMARY KEY or UNIQUE constraint. `ON t.cat =
//! s.cat` over a non-unique column is legal `MERGE` and updates every matching
//! row, which no upsert expresses, and SQLite rejects such a conflict target
//! outright with "ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE
//! constraint".
//!
//! Even when the `ON` columns are unique, the two disagree on repeated matches.
//! Given two source rows for one target row, PostgreSQL raises "MERGE command
//! cannot affect row a second time" and changes nothing, while a SQLite upsert
//! applies both in sequence and keeps the last. A translation would therefore
//! produce data PostgreSQL refuses, silently and with no error.
//!
//! `WHEN NOT MATCHED BY SOURCE THEN DELETE` removes target rows absent from the
//! source, which an insert-shaped statement cannot do at all.
//!
//! Deciding whether a given `MERGE` falls in the narrow translatable subset
//! would need the target's full index set, which the translation schema filters
//! out (plan item R83). So the check cannot even be performed today.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

const BASE: &str = "
    CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER);
    CREATE TABLE s (id INTEGER PRIMARY KEY, n INTEGER);
";

/// Translates `pg` and returns the error message, failing if translation
/// succeeds.
fn rejection(pg: &str) -> String {
    let result = Pg2Sqlite::default()
        .sql(pg)
        .expect("the fixture must parse")
        .translate(&Pg2SqliteOptions::default());

    match result {
        Ok(statements) => {
            panic!(
                "MERGE must be refused rather than dropped, but translation produced {statements:?}"
            )
        }
        Err(error) => error.to_string(),
    }
}

/// The plan's acceptance criterion: an error naming MERGE and pointing at
/// `INSERT ... ON CONFLICT`.
#[test]
fn merge_is_rejected_and_points_at_upsert() {
    let error = rejection(&format!(
        "{BASE} MERGE INTO t USING s ON t.id = s.id \
         WHEN MATCHED THEN UPDATE SET n = s.n \
         WHEN NOT MATCHED THEN INSERT (id, n) VALUES (s.id, s.n);"
    ));

    assert!(error.contains("MERGE"), "the error must name the statement, got: {error}");
    assert!(
        error.contains("ON CONFLICT"),
        "the error must point at the upsert alternative, got: {error}"
    );
}

/// An update-only `MERGE` is still refused. This is the shape closest to an
/// upsert, and it is exactly where the repeated-match divergence bites.
#[test]
fn merge_with_only_a_matched_clause_is_rejected() {
    let error = rejection(&format!(
        "{BASE} MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET n = 1;"
    ));
    assert!(error.contains("MERGE"), "the error must name the statement, got: {error}");
}

/// An insert-only `MERGE` is refused too, even though it is the one shape an
/// upsert could almost express.
#[test]
fn merge_with_only_a_not_matched_clause_is_rejected() {
    let error = rejection(&format!(
        "{BASE} MERGE INTO t USING s ON t.id = s.id \
         WHEN NOT MATCHED THEN INSERT (id, n) VALUES (s.id, s.n);"
    ));
    assert!(error.contains("MERGE"), "the error must name the statement, got: {error}");
}

/// The delete form has no insert-shaped equivalent whatsoever.
#[test]
fn merge_deleting_unmatched_target_rows_is_rejected() {
    let error = rejection(&format!(
        "{BASE} MERGE INTO t USING s ON t.id = s.id WHEN NOT MATCHED BY SOURCE THEN DELETE;"
    ));
    assert!(error.contains("MERGE"), "the error must name the statement, got: {error}");
}

/// `WHEN MATCHED THEN DO NOTHING` only became parseable in sqlparser
/// `f68211b6` (upstream #2468), which added `MergeAction::DoNothing`. It used
/// to die at parse, so nothing pinned that the new action reaches the
/// whole-statement refusal rather than slipping through.
#[test]
fn merge_with_a_do_nothing_action_is_rejected() {
    let error = rejection(&format!(
        "{BASE} MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DO NOTHING;"
    ));
    assert!(error.contains("MERGE"), "the error must name the statement, got: {error}");
}
