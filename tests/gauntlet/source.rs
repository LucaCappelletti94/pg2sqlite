//! Gauntlet A: the suite's PostgreSQL source inputs are valid PostgreSQL and
//! remain readable after they apply.
//!
//! Two groups of cases:
//!
//! - Every fixture in the shared inventory applies to a real PostgreSQL
//!   database and every table can be read as the `app` role.  Fixtures that
//!   carry a skip reason are applied too and must still fail, so the exclusion
//!   cannot outlive its cause.
//!
//! - Every corpus row is classified by two verdicts: what the translator does
//!   with it and what PostgreSQL does.  Of the four combinations, one is
//!   forbidden: the translator accepting an input PostgreSQL rejects, which
//!   would mean SQLite was built from something no real database produced.

use crate::postgres_harness;

#[path = "../helpers/emitted_corpus.rs"]
mod emitted_corpus;

use core::fmt::Write;

use emitted_corpus::{CORPUS_GROUPS, FIXTURE};
use postgres_harness::{
    CORPUS_PRELUDE, FIXTURES, apply, drop_declared_roles, fixture_source, fresh_database,
    read_every_table,
};

/// Every non-skipped fixture applies to PostgreSQL and can be read afterwards.
#[test]
fn every_fixture_applies_and_reads() {
    for fixture in FIXTURES {
        if fixture.skip.is_some() {
            continue;
        }
        let mut conn = fresh_database();
        let source = fixture_source(fixture);
        drop_declared_roles(&mut conn, &source);
        apply(&mut conn, &source)
            .unwrap_or_else(|err| panic!("{} did not apply: {err}", fixture.name));
        read_every_table(&mut conn, "app")
            .unwrap_or_else(|err| panic!("{} applied but reading failed: {err}", fixture.name));
    }
}

/// Each skipped fixture must still fail, so a skip reason cannot outlive its
/// cause.
#[test]
fn skipped_fixtures_still_refuse() {
    for fixture in FIXTURES {
        let Some(reason) = fixture.skip else { continue };
        let mut conn = fresh_database();
        let source = fixture_source(fixture);
        drop_declared_roles(&mut conn, &source);
        let still_fails = if apply(&mut conn, &source).is_err() {
            true
        } else {
            read_every_table(&mut conn, "app").is_err()
        };
        assert!(
            still_fails,
            "{} is listed as skipped but no longer fails\nskip reason: {reason}",
            fixture.name,
        );
    }
}

/// Classifies every corpus row by two verdicts: translator and PostgreSQL.
///
/// Four combinations:
///
/// - both accept: the normal case
/// - both refuse: agreement, the translator is correctly rejecting
///   non-PostgreSQL input
/// - translator refuses, PostgreSQL accepts: a SQLite limitation, the project's
///   reason to exist
/// - translator accepts, PostgreSQL refuses: forbidden, asserted empty
///
/// The forbidden bucket being non-empty means SQLite was built from something
/// no real database could have produced.  The other three counts are printed
/// so the shape of the corpus is visible as it grows.
#[test]
fn corpus_verdict_classification() {
    let opts = emitted_corpus::sweep_options();

    let mut both_accept: usize = 0;
    let mut both_refuse: usize = 0;
    let mut translator_refuses: usize = 0;
    let mut forbidden: Vec<(String, String, String)> = Vec::new();

    for (group, rows) in CORPUS_GROUPS {
        for row in *rows {
            let full = format!("{FIXTURE}\n{row}");
            let translator_accepts = crate::helpers::translate_pg(&full, &opts).is_ok();

            let mut conn = fresh_database();
            apply(&mut conn, CORPUS_PRELUDE).expect("apply corpus prelude");
            apply(&mut conn, FIXTURE).expect("apply corpus fixture");
            let pg_result = apply(&mut conn, row);
            let pg_accepts = pg_result.is_ok();

            match (translator_accepts, pg_accepts) {
                (true, true) => both_accept += 1,
                (false, false) => both_refuse += 1,
                (false, true) => translator_refuses += 1,
                (true, false) => {
                    let pg_err = pg_result.unwrap_err();
                    forbidden.push((group.to_string(), (*row).to_string(), pg_err));
                }
            }
        }
    }

    eprintln!(
        "corpus shape: both accept={both_accept}, both refuse={both_refuse}, \
         translator refuses={translator_refuses}, forbidden={}",
        forbidden.len()
    );

    if !forbidden.is_empty() {
        let mut report = String::from(
            "translator accepted inputs PostgreSQL rejects (forbidden bucket, must be empty):\n",
        );
        for (group, sql, err) in &forbidden {
            let _ = writeln!(report, "  group={group}\n  sql: {sql}\n  pg error: {err}\n");
        }
        panic!("{report}");
    }
}
