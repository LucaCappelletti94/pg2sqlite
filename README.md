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
        "INSERT INTO users (username) VALUES ('alice') ON CONFLICT DO NOTHING"
    );

    Ok(())
}
```

## Semantic differences

SQLite folds case for ASCII letters only, so `lower` and `upper` answer differently from PostgreSQL whenever the text is not ASCII. PostgreSQL folds by the database collation, which under a UTF-8 locale covers the whole of Unicode. For `ILIKE` the translator refuses a pattern literal carrying a non-ASCII letter rather than emitting a comparison that silently answers false, and `with_ilike_fold_function` names a Unicode-aware folding function (an ICU build's `lower`, or one the application registers) that `ILIKE` then runs through instead of `lower`.

```sql
SELECT 'ÄBC' ILIKE 'äbc';
-- PostgreSQL under en_US.utf8: true
-- translated without with_ilike_fold_function: refused
-- translated with it: SELECT fold('ÄBC') LIKE fold('äbc') ESCAPE '\'

SELECT lower('ÄBC');
-- PostgreSQL under en_US.utf8: äbc
-- SQLite: Äbc
```

For `lower` and `upper` two things make the engines agree. Building SQLite with `SQLITE_ENABLE_ICU` replaces them with Unicode-aware versions. Giving the PostgreSQL column or database the `C` collation makes PostgreSQL fold ASCII only, which is what SQLite already does. Non-ASCII text stored in a column still reaches an ASCII-only `lower` when `ILIKE` runs without the fold option, since only the pattern literal can be inspected at translation time.

SQLite's date functions hold milliseconds where PostgreSQL holds microseconds, so a timestamp keeps three decimal places and loses the rest. `make_time` and `make_timestamp` are exact, because they format the argument they are given rather than going through those functions.

```sql
SELECT extract(epoch from timestamp '2024-03-05 14:07:09.123456');
-- PostgreSQL: 1709647629.123456
-- SQLite:     1709647629.123
```

PostgreSQL raises an error where SQLite quietly answers something. Dividing by zero gives NULL, a cast of text that does not parse gives whatever prefix did parse, and integer arithmetic that leaves the 64-bit range degrades to a float instead of failing. None of the three has a general translation, since the SQLite answer is produced by the engine rather than by anything the emitted SQL says.

```sql
SELECT 1 / 0;
-- PostgreSQL: ERROR division by zero
-- SQLite:     NULL

SELECT CAST('12abc' AS INTEGER);
-- PostgreSQL: ERROR invalid input syntax for type integer: "12abc"
-- SQLite:     12

SELECT 9223372036854775807 + 1;
-- PostgreSQL: ERROR bigint out of range
-- SQLite:     9.22337203685478e+18, and typeof() answers 'real'
```

A `NUMERIC(p,s)` column is the exception to the third: it is stored as a scaled integer under a `CHECK` that bounds it, so `NUMERIC(10,2)` emits `CHECK (amount BETWEEN -9999999999 AND 9999999999)` and overflowing it fails. An `INTEGER` or `BIGINT` column carries no such bound.

Text comparison follows the collation. PostgreSQL uses the database's, which under a UTF-8 locale orders case-insensitively for the purpose of ranking letters, while SQLite's default `BINARY` collation compares byte by byte, so every upper-case letter sorts before every lower-case one. This reaches `ORDER BY`, `<`, `>`, `BETWEEN`, `MIN` and `MAX`, not only explicit comparisons.

```sql
SELECT 'a' < 'B';
-- PostgreSQL under en_US.utf8: true
-- SQLite:                      false
```

Declaring the PostgreSQL column or database `C` collation makes PostgreSQL compare byte by byte too, which is what SQLite already does.

`now()` becomes `datetime('now')`, which answers UTC text with whole seconds. PostgreSQL answers a `timestamp with time zone` with microseconds, so the zone, the sub-second part and the type all differ. `CURRENT_TIMESTAMP` is passed through and answers the same UTC text.

```sql
SELECT now();
-- PostgreSQL: 2026-08-08 15:08:14.548696+00
-- translated: SELECT datetime('now')  ->  2026-08-08 15:08:14
```

Two row-level security shapes cannot be reproduced by the view-and-trigger emulation, and both only arise when a row is readable under a wider predicate than it is writable. PostgreSQL lets an `UPDATE` or `DELETE` with no `WHERE` clause reach rows the session user cannot read, because nothing in the statement reads existing values. The emulated write triggers only ever fire for rows the view exposes, so such a statement affects fewer rows than PostgreSQL would. In the same situation `RETURNING` on an `UPDATE` or `DELETE` through the view can name rows the write policy skipped: SQLite reports every row the trigger fired for, while PostgreSQL reports only the rows actually changed. When every readable row is also writable, neither divergence can occur.

## License

This project is licensed under the [MIT License](https://opensource.org/licenses/MIT).
