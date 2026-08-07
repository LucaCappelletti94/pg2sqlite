//! Emits the validity-sweep corpus, translated, as SQLite floor-check scripts.
//!
//! `scripts/check_sqlite_floor.sh` appends this output to the snapshot-derived
//! corpus it already sweeps differentially against the declared floor build
//! and a recent build, so every sweep corpus row is automatically a floor
//! check. The format is the harness's own: scripts separated by a 0x01 byte,
//! each starting with a `--` label line, each self-contained. One script per
//! corpus row, carrying the translated fixture, because `runsql.c` gives every
//! script a fresh database and stops a script at its first error, so rows
//! sharing a script would mask one another.
//!
//! Refused rows emit nothing: a refusal never reaches SQLite, so it has no
//! floor to check.

use pg2sqlite::prelude::Pg2Sqlite;

include!("../tests/helpers/emitted_corpus.rs");

fn main() {
    let options = sweep_options();
    let setup: Vec<String> = Pg2Sqlite::default()
        .sql(FIXTURE)
        .expect("fixture parses")
        .translate_to_sql(&options)
        .expect("fixture translates");

    let mut scripts = Vec::new();
    for (label, cases) in CORPUS_GROUPS {
        for (index, case) in cases.iter().enumerate() {
            let full = format!("{FIXTURE}\n{case}");
            let Ok(parsed) = Pg2Sqlite::default().sql(&full) else {
                continue;
            };
            let Ok(all) = parsed.translate_to_sql(&options) else {
                continue;
            };
            let emitted = &all[setup.len().min(all.len())..];
            let statements: Vec<String> =
                setup.iter().chain(emitted.iter()).map(|s| format!("{s};")).collect();
            scripts.push(format!("-- corpus {label} {index}\n{}", statements.join("\n")));
        }
    }
    print!("{}", scripts.join("\x01"));
}
