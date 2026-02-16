//! Fuzz target for reverse SQL translation (SQLite → PostgreSQL).
//!
//! This target feeds arbitrary byte sequences through the reverse translation
//! pipeline to find crashes or panics.

use honggfuzz::fuzz;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn main() {
    // Create translator with a basic schema for reverse translation
    let schema_sql = r#"
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

    let translator = Pg2Sqlite::default().sql(schema_sql).expect("Failed to parse schema");
    let schema = translator.build_schema().expect("Failed to build schema");
    let options = Pg2SqliteOptions::default();

    loop {
        fuzz!(|data: &[u8]| {
            // Skip very long inputs to avoid excessive processing time
            if data.len() > 500 {
                return;
            }

            // Convert bytes to string, skipping invalid UTF-8
            let Ok(sql) = std::str::from_utf8(data) else {
                return;
            };

            // Try to reverse translate - we don't care about errors, only panics
            let _ = translator.reverse_sql(sql, &schema, &options);
        });
    }
}
