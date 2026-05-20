//! Build/dependency hygiene checks.
//!
//! Now that sqlparser ships releases on crates.io that we can consume
//! directly (and that sql-traits' no_std refactor also pins via
//! `sqlparser = { version = "0.62", default-features = false }`), we no
//! longer need to track upstream `main`. The invariant we want to keep is
//! that there's exactly one sqlparser version in the dep graph so AST
//! types unify between this crate and sql-traits.

#[test]
fn lockfile_has_single_sqlparser_version() {
    let cargo_lock = include_str!("../Cargo.lock");
    let sqlparser_versions = cargo_lock
        .split("\n\n")
        .filter(|chunk| chunk.contains("name = \"sqlparser\""))
        .filter_map(|chunk| {
            chunk.lines().find(|line| line.starts_with("version = ")).map(str::to_string)
        })
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        sqlparser_versions.len(),
        1,
        "Cargo.lock should contain exactly one sqlparser version so AST types \
         unify between pg2sqlite and sql-traits (found: {sqlparser_versions:?})"
    );
}

#[test]
fn cargo_toml_pins_sqlparser_to_062() {
    let cargo_toml = include_str!("../Cargo.toml");
    let sqlparser_line = cargo_toml
        .lines()
        .find(|line| line.trim_start().starts_with("sqlparser ="))
        .expect("Cargo.toml must declare sqlparser dependency");

    assert!(
        sqlparser_line.contains("version = \"0.62\""),
        "sqlparser dependency must stay version-locked to 0.62 to match \
         the sql-traits no_std baseline; line was: {sqlparser_line}"
    );
}
