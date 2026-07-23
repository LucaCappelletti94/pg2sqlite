//! Fuzz target for reverse SQL translation (SQLite to PostgreSQL).
//!
//! Feeds an `Arbitrary`-derived (options, sql) pair through the
//! reverse translation pipeline to find crashes or panics. The
//! schema is parsed once on first iteration via `LazyLock` so each
//! fuzzing step only pays for `reverse_sql` itself. Options are
//! randomised per iteration so every `with_*` toggle gets exercised
//! through the reverse path.

#![no_main]

use std::sync::LazyLock;

use libfuzzer_sys::fuzz_target;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

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

type FuzzCtx = (Pg2Sqlite, sql_traits::structs::ParserDB);

static CTX: LazyLock<FuzzCtx> = LazyLock::new(|| {
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
    if input.sql.len() > 500 {
        return;
    }
    let (translator, schema) = &*CTX;
    let Ok(stmts) = translator.reverse_sql(&input.sql, schema, &input.options) else {
        return;
    };
    // Contract: reverse output is presented as PostgreSQL, so every rendered
    // statement must reparse under PostgreSqlDialect without error.
    for stmt in &stmts {
        let rendered = stmt.to_string();
        if let Err(e) = Parser::parse_sql(&PostgreSqlDialect {}, &rendered) {
            panic!("reverse output is not valid PostgreSQL: {e}\nSQL: {rendered}");
        }
    }
});
