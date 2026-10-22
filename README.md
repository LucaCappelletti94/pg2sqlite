# pg2sqlite

[![CI](https://github.com/LucaCappelletti94/pg2sqlite/workflows/Rust%20CI/badge.svg)](https://github.com/LucaCappelletti94/pg2sqlite/actions)
[![Security Audit](https://github.com/LucaCappelletti94/pg2sqlite/workflows/Security%20Audit/badge.svg)](https://github.com/LucaCappelletti94/pg2sqlite/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Codecov](https://codecov.io/gh/LucaCappelletti94/pg2sqlite/branch/main/graph/badge.svg)](https://codecov.io/gh/LucaCappelletti94/pg2sqlite)
[![Pages](https://github.com/LucaCappelletti94/pg2sqlite/workflows/Pages/badge.svg)](https://github.com/LucaCappelletti94/pg2sqlite/actions/workflows/pages.yml)

A Rust library that translates PostgreSQL SQL into valid, runnable SQLite. It parses the PostgreSQL dialect with [`sqlparser`](https://github.com/apache/datafusion-sqlparser-rs) and emits semantically equivalent SQLite, going well past type and syntax rewriting. A live playground at [`pg2sqlite.luca.phd`](https://pg2sqlite.luca.phd) runs the translator client-side as WebAssembly against an in-page SQLite that executes the translated schema in the browser.

The contract is strict. Every returned statement is valid SQLite, an unimplemented equivalent is an explicit `Err` rather than SQL that fails at runtime, and constructs with no SQLite meaning (`CREATE FUNCTION`, `GRANT`, `COMMENT`, and similar) are dropped, reported as warnings by `translate_with_report` and left out silently by plain `translate`. Nothing that merely looks valid passes through, and the test suite checks behavior by executing the emitted SQL against SQLite rather than matching strings.

`CREATE TEMPORARY TABLE` binds definition expressions to its declared columns and preserves the temporary modifier. `WITH` preserves PostgreSQL's lexical bindings, including physical and outer references beside same-named non-recursive CTEs.

The rewrites go beyond types. Row-Level Security becomes a renamed backing table, a view enforcing the `USING` clause, and `INSTEAD OF` triggers. A GIN `to_tsvector` index becomes an FTS5 virtual table with sync triggers. pgvector maps to [sqlite-vec](https://github.com/asg017/sqlite-vec) and PostGIS to the [SQLiteGIS](https://github.com/LucaCappelletti94/sqlitegis) extension. PL/pgSQL trigger bodies become SQLite trigger syntax, and SQLite DML translates back to PostgreSQL to sync replicas upstream. The crate is `no_std + alloc` and compiles for `wasm32-unknown-unknown`.

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

    let script = Pg2Sqlite::default()
        .sql(pg_sql)?
        .translate(&Pg2SqliteOptions::default())?;

    let mut initialization = script.initialize(FreshFiles::Main)?;
    assert_eq!(
        initialization.next().expect("connection setup").to_string(),
        "PRAGMA case_sensitive_like = 1"
    );
    assert_eq!(
        initialization.next().expect("table definition").to_string(),
        "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, username TEXT NOT NULL) STRICT"
    );
    assert_eq!(
        initialization.next().expect("seed").to_string(),
        "INSERT INTO users (username) VALUES ('alice') ON CONFLICT DO NOTHING"
    );

    Ok(())
}
```

## Semantic differences

Divergences that survive translation because the difference lives in the engines, not in the emitted SQL.

- **Connection state.** The pragmas `foreign_keys = 1` and `case_sensitive_like = 1` sit in the script's `setup` part. They last only as long as the connection, so every connection that writes to the replica has to run that part, which the translation reports as a warning for foreign keys. `TEMP` objects end with the connection too, so a connection to an existing file runs `Script::connect`, which recreates them with their committed rows. A connection creating the files runs `Script::initialize` for the files it creates, and `with_attached_tables` moves the listed tables into an attached database, so their readers sit in the connection state as well.
- **Case and collation.** SQLite folds ASCII only and compares byte-wise, so `lower`, `upper`, `ORDER BY`, `MIN`, and `MAX` diverge on non-ASCII or mixed-case text and `'a' < 'B'` answers false. `ILIKE` becomes `lower(...) LIKE lower(...) ESCAPE '\'`, and a pattern literal carrying a non-ASCII letter is refused unless `with_ilike_fold_function` names a Unicode-aware fold. An ICU-built SQLite or a `C`-collation PostgreSQL agrees with the server instead.
- **Quiet answers.** PostgreSQL raises where SQLite answers. Division by zero gives `NULL`, `CAST('12abc' AS INTEGER)` gives 12, and integer overflow degrades to a float, while an `INTEGER` or `BIGINT` column carries no bound against any of it. `NUMERIC(p,s)` is the guarded exception, a scaled integer under a bounding `CHECK`, so overflowing the column fails.
- **Stored representations.** What a column physically holds is not always what PostgreSQL would hand back. `translation_manifest` answers per column as a `ColumnStorage`, one of scaled minor units, UUID bytes or canonical text, JSON array text, packed floats with their width, or `Direct`.
- **Native spatial helpers.** With `with_sqlitegis_enabled`, `CreateSpatialIndex(table, column)` and `DropSpatialIndex(table, column)` use `main`, while their three-argument forms take an explicit `main` or `temp` owner.
- **Native vector precision.** `halfvec` declarations, casts, writes and native distance operands return `UnrepresentableSemantics` because `sqlite-vec` cannot preserve binary16 storage.
- **Time.** SQLite's date functions hold milliseconds, so a timestamp keeps three decimals and loses the rest. Date, time, and interval columns hold padded text. A `timestamptz` is held as UTC text with microseconds and a `+00:00` offset, which is what `now()` and `CURRENT_TIMESTAMP` answer and what every literal or computed value written into such a column is converted to. A bound parameter is stored as bound, so the caller binds that form.
- **Transactions.** A failing statement aborts the whole transaction in PostgreSQL and leaves it open in SQLite, so applying a translated batch correctly means rolling back yourself once anything fails inside an open transaction.
- **Compound `SELECT`.** PostgreSQL resolves one type per output column and SQLite decides per value, so `SELECT 1 UNION SELECT '1'` is one row on the server and two in the replica. Cast the branches when the type matters.
- **Generated keys.** `SERIAL` and `IDENTITY` become `INTEGER PRIMARY KEY AUTOINCREMENT`. A supplied insert value moves the counter where a sequence ignores one, a rolled-back insert consumes no key, both reported as warnings, and inserts or updates that assign a `GENERATED ALWAYS` column are refused outright.
- **RLS emulation.** An unqualified `UPDATE` or `DELETE` and `RETURNING` through the view can diverge from the server only when a row is readable under a wider predicate than it is writable.

## License

This project is licensed under the [MIT License](https://opensource.org/licenses/MIT).
