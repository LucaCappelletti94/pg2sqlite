//! Round-trip equivalence fuzz target.
//!
//! Pipeline under test:
//!
//!   PG1 --forward--> SQLite1 --reverse--> PG2 --forward--> SQLite2
//!
//! The invariant we assert is **fixed-point convergence at the SQLite
//! render**: `sqlite1 == sqlite2`. If the forward translator is the
//! canonical form and the reverse translator is its true inverse on
//! the supported DML subset, one cycle round-tripping a PG2 through
//! forward should produce the same SQLite as round 1. A divergence
//! flags either:
//!
//! - the reverse translator dropping or reordering information, or
//! - the forward translator being non-deterministic on equivalent inputs.
//!
//! Comparing rendered SQLite (rather than rendered PG or AST
//! equality) sidesteps the question of "what counts as PG-level
//! equivalent" - the translator output is itself the canonical form
//! the test is anchoring on.
//!
//! The reverse step needs a schema. We reuse the same 4-table
//! fixture as `fuzz_reverse_translation` (users / posts / tags /
//! items with JSONB, UUID, vector). Inputs whose DML references
//! tables outside that schema will trip the reverse step (Err) and
//! get skipped silently - the harness's signal density depends on
//! libfuzzer eventually producing inputs that match the schema; a
//! seed corpus would amplify this materially (see follow-up note in
//! fuzz/README.md).

#![no_main]

use std::sync::LazyLock;

use libfuzzer_sys::fuzz_target;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

const SCHEMA_SQL: &str = r#"
    CREATE TABLE users (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        email TEXT,
        created_at TIMESTAMP DEFAULT NOW(),
        data JSONB
    );

    CREATE TABLE posts (
        id INTEGER PRIMARY KEY,
        user_id INTEGER REFERENCES users(id),
        title TEXT NOT NULL,
        body TEXT,
        published BOOLEAN DEFAULT FALSE
    );

    CREATE TABLE tags (
        post_id INTEGER,
        tag_id INTEGER,
        PRIMARY KEY (post_id, tag_id)
    );

    CREATE TABLE items (
        id UUID PRIMARY KEY,
        embedding vector(128),
        metadata JSONB
    );
"#;

type Schema = sql_traits::structs::ParserDB;

static SCHEMA: LazyLock<(Pg2Sqlite, Schema)> = LazyLock::new(|| {
    let translator = Pg2Sqlite::default().sql(SCHEMA_SQL).expect("schema parse");
    let schema = translator.build_schema().expect("schema build");
    (translator, schema)
});

#[derive(Debug, arbitrary::Arbitrary)]
struct FuzzInput {
    options: Pg2SqliteOptions,
    sql: String,
}

fuzz_target!(|input: FuzzInput| {
    if input.sql.len() > 1024 {
        return;
    }

    let (reverse_translator, schema) = &*SCHEMA;
    let pg1 = &input.sql;
    let options = &input.options;

    // Round 1: PG1 -> SQLite1
    let Ok(parsed1) = Pg2Sqlite::default().sql(pg1) else {
        return;
    };
    let Ok(stmts1) = parsed1.translate(options) else {
        return;
    };
    let sqlite1 = render(&stmts1);

    // Reverse: SQLite1 -> PG2 (uses the same options as forward so a
    // divergence is a real disagreement between the two translators,
    // not an artifact of two different option sets).
    let Ok(pg2_stmts) = reverse_translator.reverse_sql(&sqlite1, schema, options) else {
        return;
    };
    let pg2 = render(&pg2_stmts);

    // Round 2: PG2 -> SQLite2
    let Ok(parsed2) = Pg2Sqlite::default().sql(&pg2) else {
        return;
    };
    let Ok(stmts2) = parsed2.translate(options) else {
        return;
    };
    let sqlite2 = render(&stmts2);

    if sqlite1 != sqlite2 {
        panic!(
            "round-trip did not converge: SQLite render differs between cycle 1 and cycle 2.\n\
             \n=== Options ===\n{options:#?}\n\
             \n=== PG1 (original input) ===\n{pg1}\n\
             \n=== SQLite1 (forward round 1) ===\n{sqlite1}\n\
             \n=== PG2 (reverse of SQLite1) ===\n{pg2}\n\
             \n=== SQLite2 (forward round 2) ===\n{sqlite2}\n"
        );
    }
});

fn render(stmts: &[sqlparser::ast::Statement]) -> String {
    stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n")
}
