//! Fuzz target for reverse SQL translation (SQLite to PostgreSQL).
//!
//! Feeds arbitrary byte sequences through the reverse translation
//! pipeline to find crashes or panics. The schema is parsed once on
//! first iteration via `LazyLock` so each fuzzing step only pays for
//! `reverse_sql` itself.

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

type FuzzCtx = (Pg2Sqlite, sql_traits::structs::ParserDB, Pg2SqliteOptions);

static CTX: LazyLock<FuzzCtx> = LazyLock::new(|| {
    let translator = Pg2Sqlite::default().sql(SCHEMA_SQL).expect("schema parse");
    let schema = translator.build_schema().expect("schema build");
    let options = Pg2SqliteOptions::default();
    (translator, schema, options)
});

fuzz_target!(|data: &[u8]| {
    if data.len() > 500 {
        return;
    }
    let Ok(sql) = std::str::from_utf8(data) else {
        return;
    };
    let (translator, schema, options) = &*CTX;
    let _ = translator.reverse_sql(sql, schema, options);
});
