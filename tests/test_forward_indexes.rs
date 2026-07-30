//! Tests for forward index translation covering FTS and other index types
//! in `src/impls/translator_impls/create_index.rs`.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;
#[path = "helpers/translate.rs"]
mod translate_helpers;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;
use translate_helpers::translate_default as translate;

fn translate_result(sql: &str) -> Result<Vec<String>, String> {
    Pg2Sqlite::default()
        .sql(sql)
        .map_err(|e| e.to_string())?
        .translate(&Pg2SqliteOptions::default())
        .map(|stmts| stmts.iter().map(ToString::to_string).collect())
        .map_err(|e| e.to_string())
}

#[test]
fn basic_btree_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        CREATE INDEX idx_users_name ON users (name);
    ";
    let output = translate(sql);
    assert!(output.contains("CREATE INDEX"), "Expected CREATE INDEX: {output}");
    assert!(output.contains("idx_users_name"), "Expected index name: {output}");
}

#[test]
fn unique_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, email TEXT);
        CREATE UNIQUE INDEX idx_unique_email ON users (email);
    ";
    let output = translate(sql);
    assert!(output.contains("UNIQUE"), "Expected UNIQUE index: {output}");
}

#[test]
fn index_if_not_exists() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX IF NOT EXISTS idx_name ON users (name);
    ";
    let output = translate(sql);
    assert!(output.contains("IF NOT EXISTS"), "Expected IF NOT EXISTS: {output}");
}

#[test]
fn multi_column_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        CREATE INDEX idx_name_age ON users (name, age);
    ";
    let output = translate(sql);
    assert!(output.contains("name"), "Expected name column: {output}");
    assert!(output.contains("age"), "Expected age column: {output}");
}

#[test]
fn gin_tsvector_index_to_fts5() {
    let sql = "
        CREATE TABLE articles (id INT PRIMARY KEY, title TEXT, body TEXT);
        CREATE INDEX idx_articles_fts ON articles USING GIN (to_tsvector('english', title || ' ' || body));
    ";
    let output = translate(sql);
    // Should be translated to FTS5 virtual table or similar
    assert!(
        output.contains("fts5") || output.contains("FTS") || output.contains("articles"),
        "Expected FTS5 or table reference: {output}"
    );
}

#[test]
fn gin_tsvector_single_column() {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, content TEXT);
        CREATE INDEX idx_docs_fts ON docs USING GIN (to_tsvector('english', content));
    ";
    let output = translate(sql);
    assert!(
        output.contains("fts5") || output.contains("content") || output.contains("docs"),
        "Expected FTS5 or column reference: {output}"
    );
}

#[test]
fn gist_tsvector_index() {
    let sql = "
        CREATE TABLE articles (id INT PRIMARY KEY, title TEXT);
        CREATE INDEX idx_gist ON articles USING GiST (to_tsvector('english', title));
    ";
    let output = translate(sql);
    // GiST with tsvector should also translate to FTS5
    assert!(
        output.contains("fts5") || output.contains("articles"),
        "Expected FTS5 translation: {output}"
    );
}

#[test]
fn hash_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_hash ON users USING HASH (name);
    ";
    let output = translate(sql);
    // Hash indexes can't be directly translated - should become regular index or be
    // dropped
    assert!(output.contains("users"), "Expected table still present: {output}");
}

#[test]
fn expression_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_lower_name ON users ((lower(name)));
    ";
    let output = translate(sql);
    assert!(output.contains("users"), "Expected output: {output}");
}

#[test]
fn partial_index() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, active BOOLEAN);
        CREATE INDEX idx_active ON users (name) WHERE active = true;
    ";
    let output = translate(sql);
    assert!(
        output.contains("WHERE") || output.contains("users"),
        "Expected index or table: {output}"
    );
}

#[test]
fn concurrently_is_dropped() {
    // CONCURRENTLY is not valid in SQLite
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX CONCURRENTLY idx_name ON users (name);
    ";
    let output = translate(sql);
    assert!(!output.contains("CONCURRENTLY"), "CONCURRENTLY must be stripped: {output}");
    assert!(output.contains("idx_name"), "Index name should be preserved: {output}");
}

#[test]
fn include_clause_is_dropped() {
    // INCLUDE (covering index) is PostgreSQL-only
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        CREATE INDEX idx_name ON users (name) INCLUDE (age);
    ";
    let output = translate(sql);
    assert!(!output.contains("INCLUDE"), "INCLUDE clause must be stripped: {output}");
    assert!(output.contains("idx_name"), "Index name should be preserved: {output}");
}

#[test]
fn using_btree_is_dropped() {
    // USING BTREE is the default and is not emitted in SQLite syntax
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_name ON users USING BTREE (name);
    ";
    let output = translate(sql);
    assert!(!output.contains("USING"), "USING clause must be stripped: {output}");
    assert!(output.contains("idx_name"), "Index name should be preserved: {output}");
}

#[test]
fn gin_index_non_tsvector_causes_error() {
    let sql = "
        CREATE TABLE documents (id SERIAL PRIMARY KEY, content TEXT);
        CREATE INDEX idx_content ON documents USING GIN (content);
    ";
    let result = translate_result(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("to_tsvector"), "Error should mention to_tsvector: {}", err);
}

#[test]
fn gist_index_non_tsvector_causes_error() {
    let sql = "
        CREATE TABLE locations (id SERIAL PRIMARY KEY, point TEXT);
        CREATE INDEX idx_point ON locations USING GiST (point);
    ";
    let result = translate_result(sql);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("GiST"), "Error should mention GiST index: {}", err);
}

#[test]
fn gist_tsvector_translates_to_fts5() {
    let sql = "
        CREATE TABLE articles (id SERIAL PRIMARY KEY, title TEXT, body TEXT);
        CREATE INDEX idx_search ON articles USING GiST (to_tsvector('english', title || ' ' || body));
    ";
    let translated_sql = translate_result(sql).unwrap();

    // Should have: table + FTS5 virtual table + 3 triggers + 1 backfill INSERT
    assert_eq!(translated_sql.len(), 6);

    // First statement is the table
    assert!(translated_sql[0].contains("CREATE TABLE articles"));

    // Second statement should be CREATE VIRTUAL TABLE ... USING fts5
    assert!(
        translated_sql[1].contains("CREATE VIRTUAL TABLE"),
        "Expected FTS5 virtual table, got: {}",
        translated_sql[1]
    );
    assert!(translated_sql[1].contains("fts5"), "Expected fts5 module, got: {}", translated_sql[1]);
    assert!(
        translated_sql[1].contains("articles_fts"),
        "Expected articles_fts table name, got: {}",
        translated_sql[1]
    );
}

#[test]
fn gin_tsvector_translates_to_fts5() {
    let sql = "
        CREATE TABLE documents (id SERIAL PRIMARY KEY, title TEXT, body TEXT);
        CREATE INDEX idx_search ON documents USING GIN (to_tsvector('english', title || ' ' || body));
    ";
    let translated_sql = translate_result(sql).unwrap();

    // Should have: table + FTS5 virtual table + 3 triggers (insert, delete, update)
    // + 1 backfill INSERT
    assert_eq!(translated_sql.len(), 6);

    // First statement is the table
    assert!(translated_sql[0].contains("CREATE TABLE documents"));

    // Second statement should be CREATE VIRTUAL TABLE ... USING fts5
    assert!(
        translated_sql[1].contains("CREATE VIRTUAL TABLE"),
        "Expected FTS5 virtual table, got: {}",
        translated_sql[1]
    );
    assert!(translated_sql[1].contains("fts5"), "Expected fts5 module, got: {}", translated_sql[1]);
    assert!(
        translated_sql[1].contains("documents_fts"),
        "Expected documents_fts table name, got: {}",
        translated_sql[1]
    );
    assert!(
        translated_sql[1].contains("title"),
        "Expected title column, got: {}",
        translated_sql[1]
    );
    assert!(translated_sql[1].contains("body"), "Expected body column, got: {}", translated_sql[1]);

    // Statements 3-5 should be triggers
    assert!(
        translated_sql[2].contains("CREATE TRIGGER") && translated_sql[2].contains("AFTER INSERT"),
        "Expected INSERT trigger, got: {}",
        translated_sql[2]
    );
    assert!(
        translated_sql[3].contains("CREATE TRIGGER") && translated_sql[3].contains("AFTER DELETE"),
        "Expected DELETE trigger, got: {}",
        translated_sql[3]
    );
    assert!(
        translated_sql[4].contains("CREATE TRIGGER") && translated_sql[4].contains("AFTER UPDATE"),
        "Expected UPDATE trigger, got: {}",
        translated_sql[4]
    );
}

/// The index path had the mirror image of the table-constraint defect: it
/// cleared `nulls_distinct` unconditionally, so `NULLS NOT DISTINCT` was
/// dropped silently rather than emitted. That is worse than the syntax error,
/// because the emitted index accepts rows PostgreSQL rejects. Verified on both:
/// PostgreSQL 16 refuses a second NULL, SQLite accepts it.
#[test]
fn unique_index_nulls_not_distinct_is_rejected() {
    let error = translate_result(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
         CREATE UNIQUE INDEX i ON t (s) NULLS NOT DISTINCT;",
    )
    .expect_err("NULLS NOT DISTINCT changes which rows collide and must be reported");
    assert!(
        error.to_uppercase().contains("NULLS NOT DISTINCT"),
        "expected the error to name the clause, got {error}"
    );
}

/// The default spelling matches SQLite, so the index still translates.
#[test]
fn unique_index_nulls_distinct_is_translated() {
    let sql = translate(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
         CREATE UNIQUE INDEX i ON t (s) NULLS DISTINCT;",
    );
    assert!(
        !sql.to_uppercase().contains("NULLS"),
        "the clause has no SQLite form and must not reach the output: {sql}"
    );
}

/// SQLite rejects a `NULLS` qualifier inside an index definition, at every
/// version: measured on 3.51.1, `CREATE INDEX i ON t (n DESC NULLS LAST)`
/// answers `unsupported use of NULLS LAST`. The clause used to be copied
/// through, so the index could not be created at all.
#[test]
fn an_index_column_nulls_qualifier_is_dropped() {
    // Both NULL and non-NULL rows, so the index is exercised over the values
    // whose ordering the dropped qualifier was about.
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT);
         CREATE INDEX i ON t (n DESC NULLS LAST);
         INSERT INTO t VALUES (1, 5), (2, NULL);
         SELECT count(*) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("2".to_string())]);

    let sql = translate(
        "CREATE TABLE t (id INT PRIMARY KEY, n INT);
         CREATE INDEX i ON t (n DESC NULLS LAST);",
    );
    assert!(sql.to_uppercase().contains("DESC"), "the direction IS legal and must survive: {sql}");
}

/// Dropping it is reported, since the index then serves fewer orderings than
/// the one PostgreSQL would have built.
#[test]
fn dropping_an_index_nulls_qualifier_warns() {
    let warnings = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, n INT);
              CREATE INDEX i ON t (n DESC NULLS LAST);",
        )
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate")
        .warnings;

    assert!(
        warnings.iter().any(|warning| {
            matches!(
                warning,
                pg2sqlite::warnings::TranslationWarning::LossyDrop { construct, .. }
                    if *construct == "NULLS FIRST/LAST"
            )
        }),
        "expected a LossyDrop naming the qualifier, got {warnings:?}"
    );
}

/// An index without the qualifier loses nothing, so it must not warn. Guards
/// the warning from firing on every index.
#[test]
fn an_ordinary_index_does_not_warn() {
    let warnings = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, n INT);
              CREATE INDEX i ON t (n DESC);",
        )
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate")
        .warnings;

    assert!(warnings.is_empty(), "an ordinary index has nothing to report: {warnings:?}");
}

/// SQLite has no operator classes: `CREATE INDEX i ON t (s text_pattern_ops)`
/// answers `near "text_pattern_ops": syntax error`, so the clause was making
/// the index unbuildable.
///
/// The spelling here is UNIQUE on purpose. That is the only case where an
/// opclass could decide something other than plan shape, since PostgreSQL's
/// pattern classes compare bitwise while the default class compares by
/// collation. The two only disagree under a nondeterministic collation, and
/// `Expr::Collate` already refuses every collation but SQLite's three, all
/// deterministic, so no column that reaches SQLite can tell them apart.
#[test]
fn an_index_operator_class_is_dropped() {
    let rows = run_translated_with(
        "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
         CREATE UNIQUE INDEX i ON t (s text_pattern_ops);
         INSERT INTO t VALUES (1, 'a'), (2, 'A');
         SELECT count(*) FROM t;",
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("2".to_string())], "the two spellings must stay distinct");
}

/// Dropping it is reported, since the index then serves fewer queries than the
/// PostgreSQL one.
#[test]
fn dropping_an_index_operator_class_warns() {
    let warnings = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t (id INT PRIMARY KEY, s TEXT);
             CREATE INDEX i ON t (s text_pattern_ops);",
        )
        .expect("parse")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("translate")
        .warnings;

    assert!(
        warnings.iter().any(|warning| {
            matches!(
                warning,
                pg2sqlite::warnings::TranslationWarning::LossyDrop { construct, .. }
                    if *construct == "index operator class"
            )
        }),
        "expected a LossyDrop naming the operator class, got {warnings:?}"
    );
}
