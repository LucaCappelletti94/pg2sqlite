//! The playground keeps its own lockfile while taking this crate as a path
//! dependency, so a root pin bump that is not mirrored there compiles this
//! crate's current source against an older dependency API, and the first
//! evidence used to be a red Pages deploy (R101). This fails the drift at
//! test time instead: every git dependency the two lockfiles share must
//! agree on its pinned revision.

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

#[test]
fn the_playground_lockfile_matches_the_root_pins() {
    let root = std::fs::read_to_string("Cargo.lock").expect("the root lockfile");
    let playground = std::fs::read_to_string("examples/web-playground/Cargo.lock")
        .expect("the playground lockfile");

    let root_revisions = git_revisions(&root);
    assert!(
        !root_revisions.is_empty(),
        "the root lockfile carries git pins for sql-traits and sqlparser, so an empty map means \
         the parser broke, not that there is nothing to compare"
    );
    let playground_revisions = git_revisions(&playground);

    let disagreements: Vec<String> = root_revisions
        .iter()
        .filter_map(|(name, root_revision)| {
            let playground_revision = playground_revisions.get(name)?;
            (playground_revision != root_revision)
                .then(|| format!("{name}: root {root_revision}, playground {playground_revision}"))
        })
        .collect();

    assert!(
        disagreements.is_empty(),
        "the playground lockfile pins different git revisions than the root. Run \
         `cargo update -p <name> --precise <revision>` in examples/web-playground:\n{}",
        disagreements.join("\n")
    );
}
