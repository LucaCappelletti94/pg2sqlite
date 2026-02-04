# pg2sqlite

[![CI](https://github.com/LucaCappelletti94/pg2sqlite/workflows/Rust%20CI/badge.svg)](https://github.com/LucaCappelletti94/pg2sqlite/actions)
[![Security Audit](https://github.com/LucaCappelletti94/pg2sqlite/workflows/Security%20Audit/badge.svg)](https://github.com/LucaCappelletti94/pg2sqlite/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Codecov](https://codecov.io/gh/LucaCappelletti94/pg2sqlite/branch/main/graph/badge.svg)](https://codecov.io/gh/LucaCappelletti94/pg2sqlite)

A Rust library to translate `PostgreSQL` SQL schemas and migrations into `SQLite`-compatible SQL.

This crate uses `sqlparser` to parse `PostgreSQL` statements and then translates them into their `SQLite` equivalents, handling data type conversions and syntax differences.

## Features

- **Translation**: Converts `PostgreSQL` `CREATE TABLE`, `CREATE INDEX`, `CREATE TRIGGER`, and `INSERT` statements to `SQLite`.
- **Type Mapping**: Automatically maps `PostgreSQL` types to `SQLite` types:
  - `SERIAL`/`SMALLSERIAL` -> `INTEGER`
  - `UUID`/`BYTEA` -> `BLOB`
  - `BOOLEAN` -> `INTEGER`
  - `TIMESTAMP` -> `TEXT`
  - `GEOGRAPHY` -> `BLOB`
- **Primary Keys**: Automatically adds `NOT NULL` to all Primary Key columns.
- **UUID Generation**: Converts `PostgreSQL` default value expressions for UUIDs into pure `SQLite` equivalents:
  - `gen_random_uuid()` / `uuidv4()` -> Pure SQL UUID v4 generation (random).
  - `uuidv7()` -> Pure SQL UUID v7 generation (time-ordered).
  - Configurable representation as `BLOB` (16 bytes) or `TEXT` (36 chars).
  - Configurable extension function to use for UUID generation.
- **Migration Loading**:
  - Parse raw SQL strings.
  - Read individual SQL files.
  - Recursively load `up.sql` migration files from a directory.
  - **Git Integration**: Clone a git repository and load `up.sql` migrations directly.

## Usage

```rust
use pg2sqlite::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Define some PostgreSQL SQL
    let pg_sql = r#"
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            username TEXT NOT NULL
        );
        INSERT INTO users (username) VALUES ('alice') ON CONFLICT DO NOTHING;
    "#;

    // Translate to SQLite
    let sqlite_statements = Pg2Sqlite::default()
        .sql(pg_sql)?
        .translate(&Pg2SqliteOptions::default())?;

    // Verify the output
    // The SERIAL becomes INTEGER, and ON CONFLICT DO NOTHING becomes OR IGNORE (semantically)
    assert_eq!(sqlite_statements.len(), 2);
    
    let create_table = &sqlite_statements[0];
    assert_eq!(create_table.to_string(), "CREATE TABLE users (id INTEGER PRIMARY KEY NOT NULL, username TEXT NOT NULL) STRICT");

    let insert = &sqlite_statements[1];
    assert_eq!(insert.to_string(), "INSERT OR IGNORE INTO users (username) VALUES ('alice')");
    
    println!("SQLite Statements:\n{}\n{}", create_table, insert);
    
    Ok(())
}
```
