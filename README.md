# pg2sqlite

[![CI](https://github.com/LucaCappelletti94/pg2sqlite/workflows/Rust%20CI/badge.svg)](https://github.com/LucaCappelletti94/pg2sqlite/actions)
[![Security Audit](https://github.com/LucaCappelletti94/pg2sqlite/workflows/Security%20Audit/badge.svg)](https://github.com/LucaCappelletti94/pg2sqlite/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Codecov](https://codecov.io/gh/LucaCappelletti94/pg2sqlite/branch/main/graph/badge.svg)](https://codecov.io/gh/LucaCappelletti94/pg2sqlite)
[![Pages](https://github.com/LucaCappelletti94/pg2sqlite/workflows/Pages/badge.svg)](https://github.com/LucaCappelletti94/pg2sqlite/actions/workflows/pages.yml)

A Rust library that translates PostgreSQL SQL into valid, runnable SQLite SQL. It parses PostgreSQL-dialect statements with [`sqlparser`](https://github.com/apache/datafusion-sqlparser-rs) and emits semantically equivalent SQLite, going well beyond type and syntax rewriting.

A live playground at [`pg2sqlite.luca.phd`](https://pg2sqlite.luca.phd) runs the translator entirely client-side as WebAssembly, with an in-page SQLite so the translated schema is actually executed and queried in the browser. Paste PostgreSQL DDL, watch the SQLite translation update as you type, and run queries against the populated schema in either dialect.

The translation contract is strict. Every statement pg2sqlite returns is valid SQLite. If a construct can be translated, it is. If it has a SQLite equivalent that is not yet implemented, the call returns an explicit `Err` instead of SQL that fails at runtime. Constructs that carry no SQLite-representable information (`CREATE FUNCTION`, `GRANT`, `CREATE ROLE`, and similar) are dropped rather than passed through, as are column options with no SQLite counterpart (`COLLATION`, `CHARACTER SET`, `COMMENT`). There are no silent pass-throughs that look valid but fail on execution, and the test suite runs the translated SQL against SQLite through Diesel to check real runtime behavior rather than output strings.

Most of the interesting work is in features that SQLite can express only through non-trivial rewrites. Row-Level Security policies become a renamed backing table, a view that enforces the `USING` clause, and `INSTEAD OF` triggers, optionally tailored to a session role so a client replica only sees the data it may access. A GIN index over `to_tsvector(...)` becomes an FTS5 virtual table with sync triggers. pgvector types and distance operators map to [sqlite-vec](https://github.com/asg017/sqlite-vec), and PostGIS geometry, `ST_*` functions, and GiST indexes map to the [SQLiteGIS](https://github.com/LucaCappelletti94/sqlitegis) extension. PL/pgSQL trigger bodies are rewritten to SQLite trigger syntax, and SQLite DML can be translated back to PostgreSQL to sync replicas upstream. The crate is `no_std + alloc` and compiles for `wasm32-unknown-unknown`, so the same translator runs in a browser tab or on an embedded target.

## Quick start

```rust
use pg2sqlite::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pg_sql = "
        CREATE TABLE users (
            id SERIAL PRIMARY KEY,
            username TEXT NOT NULL
        );
        INSERT INTO users (username) VALUES ('alice') ON CONFLICT DO NOTHING;
    ";

    let sqlite_statements = Pg2Sqlite::default()
        .sql(pg_sql)?
        .translate(&Pg2SqliteOptions::default())?;

    assert_eq!(sqlite_statements.len(), 2);
    assert_eq!(
        sqlite_statements[0].to_string(),
        "CREATE TABLE users (id INTEGER PRIMARY KEY NOT NULL, username TEXT NOT NULL) STRICT"
    );
    assert_eq!(
        sqlite_statements[1].to_string(),
        "INSERT OR IGNORE INTO users (username) VALUES ('alice')"
    );

    Ok(())
}
```

## License

This project is licensed under the [MIT License](https://opensource.org/licenses/MIT).
