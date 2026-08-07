//! The playground and the fuzz workspace keep their own lockfiles while
//! taking this crate as a path dependency, so a root pin bump that is not
//! mirrored there compiles this crate's current source against an older
//! dependency API. The first evidence used to be a red Pages deploy (R101)
//! or a fuzz build failing with 92 import errors (R80 phase 3). This fails
//! the drift at test time instead: every git dependency a satellite
//! lockfile shares with the root must agree on its pinned revision.

use std::collections::BTreeMap;

/// The `name` to git `revision` map of every git-sourced package in a
/// lockfile. Cargo writes the revision after `#` in the `source` line.
fn git_revisions(lockfile: &str) -> BTreeMap<String, String> {
    let mut revisions = BTreeMap::new();
    let mut name: Option<String> = None;
    for line in lockfile.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            name = None;
        } else if let Some(rest) = line.strip_prefix("name = \"") {
            name = rest.strip_suffix('"').map(str::to_string);
        } else if line.starts_with("source = \"git+")
            && let Some((_, revision)) = line.trim_end_matches('"').rsplit_once('#')
            && let Some(name) = name.take()
        {
            revisions.insert(name, revision.to_string());
        }
    }
    revisions
}

/// Asserts every git pin `satellite` shares with the root lockfile agrees.
fn assert_lockfile_matches_root(satellite_path: &str) {
    let root = std::fs::read_to_string("Cargo.lock").expect("the root lockfile");
    let satellite = std::fs::read_to_string(satellite_path).expect("the satellite lockfile");

    let root_revisions = git_revisions(&root);
    assert!(
        !root_revisions.is_empty(),
        "the root lockfile carries git pins for sql-traits and sqlparser, so an empty map means \
         the parser broke, not that there is nothing to compare"
    );
    let satellite_revisions = git_revisions(&satellite);

    let disagreements: Vec<String> = root_revisions
        .iter()
        .filter_map(|(name, root_revision)| {
            let satellite_revision = satellite_revisions.get(name)?;
            (satellite_revision != root_revision).then(|| {
                format!("{name}: root {root_revision}, {satellite_path} {satellite_revision}")
            })
        })
        .collect();

    assert!(
        disagreements.is_empty(),
        "{satellite_path} pins different git revisions than the root. Run \
         `cargo update -p <name> --precise <revision>` against it:\n{}",
        disagreements.join("\n")
    );
}

#[test]
fn the_playground_lockfile_matches_the_root_pins() {
    assert_lockfile_matches_root("examples/web-playground/Cargo.lock");
}

#[test]
fn the_fuzz_lockfile_matches_the_root_pins() {
    assert_lockfile_matches_root("fuzz/Cargo.lock");
}
