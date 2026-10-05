//! Regression tests for crash cases found by fuzzing.
//!
//! This test:
//! 1. Copies any new crash files from
//!    `fuzz/hfuzz_workspace/fuzz_sql_translation/*.fuzz` into
//!    `tests/crash_cases/`
//! 2. Runs all crash cases from `tests/crash_cases/`
//!
//! Crash files in `tests/crash_cases/` should be committed to git for
//! regression testing.

use std::{fs, path::Path};

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

/// Copy new crash files from fuzzer workspace to tests/crash_cases/.
fn copy_new_crashes() {
    let fuzz_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/hfuzz_workspace/fuzz_sql_translation");
    let crash_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/crash_cases");

    // Create crash_cases directory if it doesn't exist
    if !crash_dir.exists() {
        fs::create_dir_all(&crash_dir).expect("Failed to create crash_cases directory");
    }

    // Skip if fuzzer hasn't been run
    if !fuzz_dir.exists() {
        return;
    }

    // Copy any .fuzz files that don't already exist in crash_cases
    if let Ok(entries) = fs::read_dir(&fuzz_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "fuzz") {
                let filename = path.file_name().unwrap();
                let dest = crash_dir.join(filename);
                if !dest.exists()
                    && let Err(e) = fs::copy(&path, &dest)
                {
                    eprintln!("Failed to copy {}: {}", path.display(), e);
                }
            }
        }
    }
}

/// Run a single crash case through the pipeline.
/// Handles bytes the same way as the fuzzer - skips invalid UTF-8.
fn run_crash_case(data: &[u8]) {
    // Skip invalid UTF-8, same as the fuzzer
    let Ok(sql) = std::str::from_utf8(data) else {
        return;
    };

    // Try to parse and translate - we don't care about errors, only panics
    if let Ok(parsed) = Pg2Sqlite::default().sql(sql) {
        let options = Pg2SqliteOptions::default();
        let _ = parsed.translate(&options);
    }
}

#[test]
fn test_all_crash_cases() {
    // First, copy any new crashes from the fuzzer
    copy_new_crashes();

    // Now run all crash cases from tests/crash_cases/
    let crash_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/crash_cases");

    if !crash_dir.exists() {
        return;
    }

    let entries: Vec<_> = fs::read_dir(&crash_dir)
        .expect("Failed to read crash_cases directory")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .collect();

    if entries.is_empty() {
        return;
    }

    let mut failures = Vec::new();

    for entry in entries {
        let path = entry.path();
        let filename = path.file_name().unwrap().to_string_lossy().to_string();

        let content = match fs::read(&path) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{filename}: failed to read: {e}"));
                continue;
            }
        };

        // Use catch_unwind to detect panics
        let result = std::panic::catch_unwind(|| {
            run_crash_case(&content);
        });

        if let Err(panic) = result {
            let panic_msg = if let Some(s) = panic.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            failures.push(format!("{filename}: panicked: {panic_msg}"));
        }
    }

    assert!(failures.is_empty(), "Crash case regressions detected:\n{}", failures.join("\n"));
}
