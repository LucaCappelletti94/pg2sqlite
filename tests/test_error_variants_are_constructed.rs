//! Every public `Error` variant has to be reachable.
//!
//! A variant nothing constructs is public API for a feature that does not
//! exist. A caller can match on it, will never see it fire, and has no way to
//! tell that from a case that simply has not happened yet. The compiler says
//! nothing, because `dead_code` does not fire on a `pub enum`.
//!
//! This reads the source rather than the type, because Rust has no reflection
//! over enum variants and the property being checked is exactly "does any code
//! name this".

use std::path::Path;

/// Variants no code names because `#[from]` builds them, so `?` is the only
/// construction site and it never spells the variant out.
const BUILT_BY_QUESTION_MARK: &[&str] =
    &["SchemaError", "LookupError", "IoError", "SqlParse", "TranslationRefusal"];

/// The variant names declared by `pub enum Error`.
///
/// A variant is a line indented exactly four spaces whose first character is
/// an uppercase letter. Doc comments and attributes at that depth start with
/// `/` and `#`, and variant bodies are indented deeper, so this separates them
/// without needing to parse Rust.
fn declared_variants(source: &str) -> Vec<&str> {
    let source = source
        .split_once("pub enum Error {")
        .expect("Error enum must exist")
        .1
        .split_once("\n}\nimpl Error")
        .expect("Error impl must follow the enum")
        .0;
    source
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("    ")?;
            let first = rest.chars().next()?;
            if !first.is_ascii_uppercase() {
                return None;
            }
            let end = rest.find(|c: char| !c.is_alphanumeric() && c != '_')?;
            Some(&rest[..end])
        })
        .collect()
}

/// Every `.rs` file under `dir`, read into one string, skipping `except`.
fn sources_under(dir: &Path, except: &Path) -> String {
    let mut combined = String::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).expect("src should be readable") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") && path != except {
                combined.push_str(&std::fs::read_to_string(&path).expect("a source file"));
            }
        }
    }
    combined
}

#[test]
fn every_error_variant_is_constructed_somewhere() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let definition = root.join("src/errors.rs");
    let source = std::fs::read_to_string(&definition).expect("errors.rs should be readable");

    let variants = declared_variants(&source);
    assert!(variants.len() > 10, "the variant scan found only {variants:?}, so it is broken");

    let rest = sources_under(&root.join("src"), &definition);
    let dead: Vec<&str> = variants
        .into_iter()
        .filter(|variant| !BUILT_BY_QUESTION_MARK.contains(variant))
        .filter(|variant| !rest.contains(&format!("Error::{variant}")))
        .collect();

    assert!(
        dead.is_empty(),
        "these Error variants are never constructed, so no caller can ever see them: {dead:?}. \
         Construct them where they belong, or remove them."
    );
}
