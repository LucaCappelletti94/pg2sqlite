//! Build/dependency hygiene checks.

#[test]
fn lockfile_has_single_sqlparser_git_source() {
    let cargo_lock = include_str!("../Cargo.lock");
    let sqlparser_git_sources = cargo_lock
        .lines()
        .filter(|line| {
            line.contains("source = \"git+https://github.com/apache/datafusion-sqlparser-rs")
        })
        .count();

    assert_eq!(
        sqlparser_git_sources, 1,
        "Cargo.lock should contain exactly one sqlparser git source to avoid duplicate sqlparser types"
    );
}

#[test]
fn cargo_toml_keeps_sqlparser_on_upstream_git_main() {
    let cargo_toml = include_str!("../Cargo.toml");
    let sqlparser_line = cargo_toml
        .lines()
        .find(|line| line.trim_start().starts_with("sqlparser ="))
        .expect("Cargo.toml must declare sqlparser dependency");

    assert!(
        sqlparser_line.contains("git = \"https://github.com/apache/datafusion-sqlparser-rs\""),
        "sqlparser must remain sourced from the datafusion-sqlparser-rs git repository"
    );
    assert!(
        sqlparser_line.contains("branch = \"main\""),
        "sqlparser must continue using the upstream `main` branch until a release is published"
    );
    assert!(
        !sqlparser_line.contains("rev = \""),
        "sqlparser dependency line should not drift between branch and rev pinning"
    );
}
