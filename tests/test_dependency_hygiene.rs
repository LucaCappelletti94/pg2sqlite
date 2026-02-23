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
