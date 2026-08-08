//! The crate's central guarantee, enforced over a corpus of PostgreSQL
//! constructs: if translation succeeds, SQLite must accept the result.
//!
//! For every construct there are exactly two acceptable outcomes:
//!
//! * translation returns `Err`, because the construct has no SQLite form, or
//! * translation succeeds and SQLite prepares every emitted statement.
//!
//! Emitting SQL that SQLite cannot parse, or naming a function it does not
//! have, is the failure this test exists to catch. Panicking is also a failure:
//! the public API returns `Result` and must never unwind.
//!
//! Constructs are executed as generated text through `rusqlite` because the
//! string the translator produced is the artifact under test.
//!
//! The corpus itself lives in `tests/helpers/emitted_corpus.rs`, shared with
//! `examples/floor_corpus.rs`, which routes every row's translation through
//! the SQLite floor harness. A row added there is therefore both
//! validity-swept here and floor-checked in CI.

#[path = "helpers/emitted_corpus.rs"]
mod emitted_corpus;
#[path = "helpers/statistical_aggregates.rs"]
mod statistical_aggregates;

use core::sync::atomic::{AtomicU64, Ordering};

use emitted_corpus::{CORPUS_GROUPS, FIXTURE, row_statements, sweep_options};
use pg2sqlite::prelude::{Pg2Sqlite, TranslationOptions};
use rusqlite::{Connection, functions::FunctionFlags};
use statistical_aggregates::{STATISTICAL_AGGREGATES, register_statistical_aggregates};

/// A SQLite complaint that means the translator emitted something wrong, as
/// opposed to the corpus referencing a name it never declared.
fn is_translator_fault(msg: &str) -> bool {
    msg.contains("syntax error") || msg.contains("unrecognized token") || msg.contains("no such")
}

/// The corpus rows over `var_pop` and friends only reach SQLite because the
/// sweep options declare those names, and they only run because this
/// connection carries them. Either half alone would make the rows assert
/// nothing.
#[test]
fn the_sweep_declares_every_registered_aggregate() {
    let options = sweep_options();
    for name in STATISTICAL_AGGREGATES {
        assert!(
            options.declares_user_defined_function(name),
            "sweep_options must declare {name}, which the sweep connection registers"
        );
    }
}

fn sqlite_accepts(setup: &[String], stmts: &[String]) -> Result<(), String> {
    let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
    // Register sqrt and pow so the sweep can validate SQL emitted when math
    // functions are enabled in the sweep options. Both are needed because ^ and
    // ||/ translate to pow().
    conn.create_scalar_function("sqrt", 1, FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        let x: f64 = ctx.get(0)?;
        Ok(x.sqrt())
    })
    .map_err(|e| format!("sqrt UDF registration failed: {e}"))?;
    conn.create_scalar_function("pow", 2, FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        let base: f64 = ctx.get(0)?;
        let exp: f64 = ctx.get(1)?;
        Ok(base.powf(exp))
    })
    .map_err(|e| format!("pow UDF registration failed: {e}"))?;
    register_statistical_aggregates(&conn)
        .map_err(|e| format!("statistical aggregate registration failed: {e}"))?;
    // The two UUID generators. Distinct sixteen-byte values, because the
    // emitted column carries a length CHECK and often a primary key, so a
    // constant would pass the first insert and fail the second for a reason
    // that has nothing to do with the translation.
    for name in ["uuid", "uuid7"] {
        conn.create_scalar_function(name, 0, FunctionFlags::default(), |_| {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let mut value = [0u8; 16];
            value[8..].copy_from_slice(&NEXT.fetch_add(1, Ordering::Relaxed).to_be_bytes());
            Ok(value.to_vec())
        })
        .map_err(|e| format!("{name} UDF registration failed: {e}"))?;
    }
    for s in setup {
        conn.execute_batch(&format!("{s};")).map_err(|e| format!("setup rejected: {e}"))?;
    }
    for s in stmts {
        // `prepare` checks syntax, function names, and column names without
        // running anything. DDL and trigger bodies are only checked when
        // executed, and a case may carry its own DDL that later statements
        // in the same case depend on, so every statement is also executed.
        // Execution additionally catches names SQLite resolves lazily, a
        // trigger body's functions being the R99 example. A non-fault
        // execution error (a constraint, a data error) is not this test's
        // concern.
        if let Err(prepare_err) = conn.prepare(s) {
            let msg = prepare_err.to_string();
            if is_translator_fault(&msg) {
                return Err(format!("{msg}\n         emitted: {s}"));
            }
        }
        if let Err(exec_err) = conn.execute_batch(&format!("{s};")) {
            let msg = exec_err.to_string();
            if is_translator_fault(&msg) {
                return Err(format!("{msg}\n         emitted: {s}"));
            }
        }
    }
    Ok(())
}

/// Translate each case against the fixture and report the ones SQLite rejects.
fn sweep(label: &str, cases: &[&str]) -> Vec<String> {
    let opts = sweep_options();
    let setup: Vec<String> = Pg2Sqlite::default()
        .sql(FIXTURE)
        .expect("fixture parses")
        .translate_to_sql(&opts)
        .expect("fixture translates");

    let mut bugs = Vec::new();
    for case in cases {
        let full = format!("{FIXTURE}\n{case}");
        let outcome = std::panic::catch_unwind(|| {
            Pg2Sqlite::default().sql(&full).and_then(|p| p.translate_to_sql(&opts))
        });
        let all = match outcome {
            // A panic escaping the public API is a defect regardless of what
            // the construct is.
            Err(_) => {
                bugs.push(format!("[{label}] {case}\n      -> PANIC, must return Err"));
                continue;
            }
            // An explicit rejection is a valid outcome.
            Ok(Err(_)) => continue,
            Ok(Ok(all)) => all,
        };
        let emitted = row_statements(&setup, &all);
        if let Err(why) = sqlite_accepts(&setup, &emitted) {
            bugs.push(format!("[{label}] {case}\n      -> {why}"));
        }
    }
    bugs
}

/// The corpus group named `label`, which must exist.
fn group(label: &str) -> (&'static str, &'static [&'static str]) {
    CORPUS_GROUPS
        .iter()
        .copied()
        .find(|(name, _)| *name == label)
        .unwrap_or_else(|| panic!("no corpus group named {label}"))
}

fn report(labels: &[&str]) {
    let mut bugs = Vec::new();
    for label in labels {
        let (name, cases) = group(label);
        bugs.extend(sweep(name, cases));
    }
    assert!(
        bugs.is_empty(),
        "{} construct(s) produced SQL SQLite rejects:\n\n{}\n",
        bugs.len(),
        bugs.join("\n")
    );
}

/// Every group in `CORPUS_GROUPS` is swept by exactly one test here, so a
/// group added to the corpus without a sweep would be dead data.
#[test]
fn every_corpus_group_is_swept() {
    let swept: Vec<&str> = [
        vec!["dml"],
        vec!["query"],
        vec!["window"],
        vec!["operator"],
        vec!["json"],
        vec!["expr"],
        vec!["func"],
        vec!["ddl"],
        vec!["types"],
        REMEDIATION_GROUPS.to_vec(),
        REVIEW_GROUPS.to_vec(),
    ]
    .concat();
    for (name, _) in CORPUS_GROUPS {
        assert!(swept.contains(name), "corpus group {name} is not swept by any test");
    }
}

#[test]
fn dml_constructs() {
    report(&["dml"]);
}

#[test]
fn query_constructs() {
    report(&["query"]);
}

#[test]
fn window_constructs() {
    report(&["window"]);
}

#[test]
fn operator_constructs() {
    report(&["operator"]);
}

#[test]
fn json_operator_constructs() {
    report(&["json"]);
}

#[test]
fn expression_constructs() {
    report(&["expr"]);
}

#[test]
fn function_constructs() {
    report(&["func"]);
}

#[test]
fn ddl_constructs() {
    report(&["ddl"]);
}

#[test]
fn type_constructs() {
    report(&["types"]);
}

/// The groups added by the 2026-07-29 remediation: one corpus row per
/// construct class it touched (R80 phase 1) plus the phase 2 conversions.
/// Refusals are valid outcomes here like everywhere in this file: a refusal
/// row pins that the construct stays refused rather than regressing into an
/// emission.
const REMEDIATION_GROUPS: &[&str] = &[
    "phase3",
    "numeric",
    "remediation-ddl",
    "trigger",
    "scalar-srf",
    "foreign-clause",
    "window-over",
    "rls-delete",
    "quantifier",
    "operators-phase2",
    "rls-policies-phase2",
];

#[test]
fn remediation_constructs() {
    report(REMEDIATION_GROUPS);
}

/// The groups added by the 2026-08-07 crate review, one per finding whose fix
/// changed what reaches SQLite.
const REVIEW_GROUPS: &[&str] = &[
    "date-arithmetic",
    "like-escape",
    "interval-arithmetic",
    "on-conflict-do-nothing",
    "foreign-key-match",
    "serial-columns",
    "boolean-to-text",
    "rls-view-reads",
    "plpgsql-scanner-and-binding",
    "statistical-aggregates",
    "uuid-version",
    "case-folding",
    "subsecond-precision",
    "cube-root",
];

#[test]
fn review_finding_constructs() {
    report(REVIEW_GROUPS);
}
