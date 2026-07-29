//! Tests verifying that domain-specific custom types (cas, molecularformula,
//! mediatype) translate to BLOB, not BINARY.  BINARY is not a valid column type
//! in SQLite STRICT tables; only INT, INTEGER, REAL, TEXT, BLOB, and ANY are
//! permitted.

#![allow(missing_docs)]

#[path = "helpers/translate.rs"]
mod translate_helpers;
use translate_helpers::translate_default as translate;

#[test]
fn cas_type_maps_to_blob_not_binary() {
    let sql = "CREATE TABLE compounds (id INT PRIMARY KEY, formula cas);";
    let out = translate(sql);
    assert!(out.contains("BLOB"), "cas should map to BLOB: {out}");
    assert!(!out.contains("BINARY"), "cas must not produce BINARY (invalid in STRICT): {out}");
}

#[test]
fn molecular_formula_maps_to_blob_not_binary() {
    let sql = "CREATE TABLE substances (id INT PRIMARY KEY, mf molecularformula);";
    let out = translate(sql);
    assert!(out.contains("BLOB"), "molecularformula should map to BLOB: {out}");
    assert!(!out.contains("BINARY"), "molecularformula must not produce BINARY: {out}");
}

#[test]
fn media_type_maps_to_blob_not_binary() {
    let sql = "CREATE TABLE attachments (id INT PRIMARY KEY, mime mediatype);";
    let out = translate(sql);
    assert!(out.contains("BLOB"), "mediatype should map to BLOB: {out}");
    assert!(!out.contains("BINARY"), "mediatype must not produce BINARY: {out}");
}
