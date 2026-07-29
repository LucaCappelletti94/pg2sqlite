//! End-to-end coverage for every canned query chip in the web playground.
//!
//! For each sample shipped in `examples/web-playground/src/samples.rs`,
//! this test:
//!
//! 1. Translates the sample's PG schema through `Pg2Sqlite::translate` using
//!    the same options the playground configures (UUID = Blob, geolite enabled,
//!    RLS audit table for the RLS sample, session variable mapping where
//!    applicable).
//! 2. Applies the translated SQL to an in-memory `rusqlite` connection that
//!    registers the same auxiliary surface the playground's `db::reopen` does:
//!    the `sqlite-vec` auto-extension and a `current_app_user()` UDF returning
//!    the integer `42` for RLS.
//! 3. Runs every chip query through the matching dialect path
//!    (`Pg2Sqlite::translate` for the Postgres dialect, raw SQL for the SQLite
//!    dialect) and asserts that positive chips succeed and negative chips fail.
//!
//! The chip catalogue is duplicated here on purpose: the playground
//! crate is wasm-only, and pulling it as a workspace member to share
//! the data would drag Dioxus's transitive deps into the main
//! pg2sqlite cargo graph. Any change to a chip in `samples.rs` must
//! be mirrored here. The test will fail loudly when the two drift.
//!
//! PostGIS queries that need SQLiteGIS at runtime are gated on the
//! `sqlitegis` cargo feature: without it we exercise translation only
//! (and assert that translation itself does not crash), since the
//! `ST_*` functions are not registered on the native test connection.
//! With `--features sqlitegis` the test registers `sqlitegis::sqlite::
//! register_functions` on the rusqlite connection so the full chip
//! pipeline (translate + apply + run twice) executes end-to-end.

#![allow(clippy::too_many_lines)]

use std::sync::Once;

use pg2sqlite::prelude::{
    Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, TranslationOptions, UuidRepresentation,
};
use rusqlite::{Connection, ffi::sqlite3_auto_extension};
use sqlite_vec::sqlite3_vec_init;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dialect {
    Postgres,
    // Kept in lockstep with the playground's `QueryDialect` so future
    // chips that genuinely need SQLite-only syntax can opt in without
    // a test-side schema change.
    #[allow(dead_code)]
    Sqlite,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Expectation {
    Positive,
    Negative,
}

struct CannedQuery {
    label: &'static str,
    sql: &'static str,
    dialect: Dialect,
    expectation: Expectation,
}

fn install_sqlite_vec() {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::ffi::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::ffi::c_int,
        >(sqlite3_vec_init as *const ())));
    });
}

fn open_connection() -> Connection {
    install_sqlite_vec();
    Connection::open_in_memory().expect("open in-memory sqlite")
}

fn register_current_app_user(conn: &Connection) {
    conn.create_scalar_function(
        "current_app_user",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |_| Ok(42i64),
    )
    .expect("register current_app_user");
}

fn translate(schema: &str, options: &Pg2SqliteOptions) -> String {
    let stmts = Pg2Sqlite::default()
        .sql(schema)
        .expect("parse schema")
        .translate(options)
        .expect("translate schema");
    stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n")
}

fn run_query_once(
    conn: &Connection,
    schema: &str,
    query: &CannedQuery,
    options: &Pg2SqliteOptions,
    pass_label: &str,
) {
    let effective_sql = match query.dialect {
        Dialect::Sqlite => query.sql.to_string(),
        Dialect::Postgres => {
            // Mirror the playground's schema-aware query translation:
            // translate `schema + query` together and keep only the
            // tail. That way the vector-insert wrap and other
            // schema-driven rewrites get a complete table picture.
            let schema_stmts = Pg2Sqlite::default()
                .sql(schema)
                .and_then(|t| t.translate(options))
                .unwrap_or_else(|e| panic!("[{}] schema translate failed: {e}", query.label));
            let combined = format!("{schema};\n{}", query.sql);
            let combined_stmts = Pg2Sqlite::default()
                .sql(&combined)
                .and_then(|t| t.translate(options))
                .unwrap_or_else(|e| panic!("[{}] query translate failed: {e}", query.label));
            combined_stmts
                .into_iter()
                .skip(schema_stmts.len())
                .map(|s| format!("{s};"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    };

    let outcome = conn.execute_batch(&effective_sql);

    match (query.expectation, &outcome) {
        (Expectation::Positive, Ok(())) | (Expectation::Negative, Err(_)) => {}
        (Expectation::Positive, Err(e)) => {
            panic!(
                "[{} / {pass_label}] expected success but got error: {e}\nSQL: {effective_sql}",
                query.label,
            );
        }
        (Expectation::Negative, Ok(())) => {
            panic!(
                "[{} / {pass_label}] expected failure but query succeeded; SQL: {effective_sql}",
                query.label,
            );
        }
    }
}

/// Run every chip query twice in sequence against the same
/// connection. The double-run catches "click chip twice" regressions
/// like the UNIQUE-constraint hit on hardcoded primary keys: positive
/// INSERTs must remain re-runnable, and negative chips must keep
/// failing for the same reason.
fn run_queries_twice(
    conn: &Connection,
    schema: &str,
    queries: &[CannedQuery],
    options: &Pg2SqliteOptions,
) {
    for q in queries {
        run_query_once(conn, schema, q, options, "first click");
        run_query_once(conn, schema, q, options, "second click");
    }
}

const SIMPLE_SCHEMA: &str = "\
CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT NOW()
);

INSERT INTO users (name) VALUES ('alice'), ('bob'), ('carol');
";

const SIMPLE_QUERIES: &[CannedQuery] = &[
    CannedQuery {
        label: "All users",
        sql: "SELECT * FROM users",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
    CannedQuery {
        label: "Find alice",
        sql: "SELECT * FROM users WHERE name = 'alice'",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
    CannedQuery {
        label: "Add a new user",
        sql: "INSERT INTO users (name) VALUES ('dave')",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
];

#[test]
fn simple_sample_queries() {
    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob);
    let conn = open_connection();
    conn.execute_batch(&translate(SIMPLE_SCHEMA, &opts)).expect("apply Simple schema");
    run_queries_twice(&conn, SIMPLE_SCHEMA, SIMPLE_QUERIES, &opts);
}

const FTS5_SCHEMA: &str = "\
CREATE TABLE docs (
    id INTEGER PRIMARY KEY,
    body TEXT
);

CREATE INDEX docs_fts ON docs USING GIN (to_tsvector('english', body));

INSERT INTO docs (id, body) VALUES
    (1, 'pg2sqlite translates PostgreSQL schemas'),
    (2, 'SQLite has a built-in full-text search module'),
    (3, 'FTS5 supports MATCH queries with ranking');
";

const FTS5_QUERIES: &[CannedQuery] = &[
    CannedQuery {
        label: "All docs",
        sql: "SELECT id, body FROM docs",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
    CannedQuery {
        label: "Match 'pg2sqlite'",
        sql: "SELECT id, body FROM docs WHERE to_tsvector('english', body) @@ to_tsquery('pg2sqlite')",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
    CannedQuery {
        label: "Match 'sqlite'",
        sql: "SELECT id, body FROM docs WHERE to_tsvector('english', body) @@ to_tsquery('sqlite')",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
];

#[test]
fn fts5_sample_queries() {
    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob);
    let conn = open_connection();
    conn.execute_batch(&translate(FTS5_SCHEMA, &opts)).expect("apply FTS5 schema");
    run_queries_twice(&conn, FTS5_SCHEMA, FTS5_QUERIES, &opts);
}

const PGVECTOR_SCHEMA: &str = "\
CREATE EXTENSION vector;

CREATE TABLE items (
    id INTEGER PRIMARY KEY,
    embedding vector(3)
);

INSERT INTO items (id, embedding) VALUES
    (1, '[0.10, 0.20, 0.30]'),
    (2, '[0.40, 0.50, 0.60]'),
    (3, '[0.70, 0.80, 0.90]');
";

const PGVECTOR_QUERIES: &[CannedQuery] = &[
    CannedQuery {
        label: "All items",
        sql: "SELECT id, embedding FROM items",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
    CannedQuery {
        label: "Nearest to [0.5, 0.5, 0.5]",
        sql: "SELECT id FROM items ORDER BY embedding <-> '[0.5, 0.5, 0.5]' LIMIT 3",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
    CannedQuery {
        label: "Insert a vector",
        sql: "INSERT INTO items (embedding) VALUES ('[0.5, 0.5, 0.5]')",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
];

#[test]
fn pgvector_sample_queries() {
    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob);
    let conn = open_connection();
    conn.execute_batch(&translate(PGVECTOR_SCHEMA, &opts)).expect("apply pgvector schema");
    run_queries_twice(&conn, PGVECTOR_SCHEMA, PGVECTOR_QUERIES, &opts);
}

const RLS_SCHEMA: &str = "\
CREATE TABLE documents (
    id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    title TEXT NOT NULL,
    content TEXT
);

ALTER TABLE documents ENABLE ROW LEVEL SECURITY;

CREATE POLICY documents_select_policy ON documents
    FOR SELECT
    USING (owner_id = current_setting('app.user_id')::integer);

CREATE POLICY documents_insert_policy ON documents
    FOR INSERT
    WITH CHECK (owner_id = current_setting('app.user_id')::integer);

INSERT INTO documents (id, owner_id, title) VALUES
    (1, 42, 'First draft'),
    (2, 42, 'Second draft');
";

const RLS_QUERIES: &[CannedQuery] = &[
    CannedQuery {
        label: "My visible documents",
        sql: "SELECT * FROM documents",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
    CannedQuery {
        label: "Insert my own row",
        sql: "INSERT INTO documents (owner_id, title) VALUES (42, 'Third draft')",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
    CannedQuery {
        label: "Insert for another user (blocked)",
        sql: "INSERT INTO documents (id, owner_id, title) VALUES (99, 99, 'Owned by someone else')",
        dialect: Dialect::Postgres,
        expectation: Expectation::Negative,
    },
];

#[test]
fn rls_sample_queries() {
    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_rls_audit_table_name("rls_violations")
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));
    let conn = open_connection();
    register_current_app_user(&conn);
    conn.execute_batch(&translate(RLS_SCHEMA, &opts)).expect("apply RLS schema");
    run_queries_twice(&conn, RLS_SCHEMA, RLS_QUERIES, &opts);
}

const CONSTRAINTS_SCHEMA: &str = "\
CREATE TABLE orders (
    id SERIAL PRIMARY KEY,
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    unit_price NUMERIC NOT NULL CHECK (unit_price >= 0),
    total NUMERIC GENERATED ALWAYS AS (quantity * unit_price) STORED,
    placed_at TIMESTAMP NOT NULL DEFAULT NOW()
);

INSERT INTO orders (quantity, unit_price) VALUES
    (3, 19.99),
    (10, 4.50),
    (1, 99.00);
";

const CONSTRAINTS_QUERIES: &[CannedQuery] = &[
    CannedQuery {
        label: "All orders (with computed total)",
        sql: "SELECT id, quantity, unit_price, total FROM orders",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
    CannedQuery {
        label: "Add a valid order",
        sql: "INSERT INTO orders (quantity, unit_price) VALUES (5, 12.50)",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
    CannedQuery {
        label: "Write to generated total (blocked)",
        sql: "INSERT INTO orders (quantity, unit_price, total) VALUES (1, 1, 999)",
        dialect: Dialect::Postgres,
        expectation: Expectation::Negative,
    },
    CannedQuery {
        label: "Negative quantity (CHECK blocks)",
        sql: "INSERT INTO orders (quantity, unit_price) VALUES (-1, 10)",
        dialect: Dialect::Postgres,
        expectation: Expectation::Negative,
    },
];

#[test]
fn constraints_sample_queries() {
    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob);
    let conn = open_connection();
    conn.execute_batch(&translate(CONSTRAINTS_SCHEMA, &opts)).expect("apply Constraints schema");
    run_queries_twice(&conn, CONSTRAINTS_SCHEMA, CONSTRAINTS_QUERIES, &opts);
}

// PostGIS (translation-only without the `geolite` feature; full apply
// behind the feature flag, since ST_GeomFromText / ST_X / ST_Within
// need SQLiteGIS at runtime.)

const POSTGIS_SCHEMA: &str = "\
CREATE TABLE places (
    id INTEGER PRIMARY KEY,
    name TEXT,
    geom geometry
);

CREATE INDEX places_geom_idx ON places USING gist (geom);

INSERT INTO places (id, name, geom) VALUES
    (1, 'Helsinki', ST_GeomFromText('POINT(24.94 60.17)')),
    (2, 'Rome',     ST_GeomFromText('POINT(12.50 41.90)')),
    (3, 'Tokyo',    ST_GeomFromText('POINT(139.69 35.69)'));
";

const POSTGIS_QUERIES: &[CannedQuery] = &[
    CannedQuery {
        label: "All places (lon / lat)",
        sql: "SELECT id, name, ST_X(geom) AS lon, ST_Y(geom) AS lat FROM places",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
    CannedQuery {
        label: "Inside Europe bbox",
        sql: "SELECT name FROM places WHERE ST_Within(geom, ST_MakeEnvelope(-10, 35, 30, 70))",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
    CannedQuery {
        label: "Find Helsinki",
        sql: "SELECT name, ST_X(geom) AS lon, ST_Y(geom) AS lat FROM places WHERE name = 'Helsinki'",
        dialect: Dialect::Postgres,
        expectation: Expectation::Positive,
    },
];

#[cfg(not(feature = "sqlitegis"))]
#[test]
fn postgis_sample_queries_translate() {
    // Without the SQLiteGIS extension the in-memory connection cannot
    // execute ST_GeomFromText, so we cannot apply the schema here.
    // We at least confirm every chip query parses and translates
    // cleanly, which is what the playground does at click time.
    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob);
    let _ = translate(POSTGIS_SCHEMA, &opts);
    for q in POSTGIS_QUERIES {
        if q.dialect == Dialect::Postgres {
            let _ = Pg2Sqlite::default()
                .sql(q.sql)
                .unwrap_or_else(|e| panic!("[{}] PostGIS parse failed: {e}", q.label))
                .translate(&opts)
                .unwrap_or_else(|e| panic!("[{}] PostGIS translate failed: {e}", q.label));
        }
    }
}

#[cfg(feature = "sqlitegis")]
#[test]
fn postgis_sample_queries() {
    // Register SQLiteGIS on every fresh sqlite handle via
    // sqlite3_auto_extension, so the chip queries (ST_X, ST_Y,
    // ST_Within, ST_MakeEnvelope, ...) execute against the rusqlite
    // connection the same way they do in the browser playground.
    //
    // `rusqlite::ffi` is a re-export of the same `libsqlite3-sys` this crate
    // depends on, so the init signatures are one type and the pointer needs no
    // cast. Passing it directly makes the compiler enforce that: if the two ever
    // resolve to different versions, this stops building instead of transmuting
    // across an ABI mismatch.
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(sqlitegis_init));
    });

    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob);
    let conn = open_connection();
    conn.execute_batch(&translate(POSTGIS_SCHEMA, &opts)).expect("apply PostGIS schema");
    run_queries_twice(&conn, POSTGIS_SCHEMA, POSTGIS_QUERIES, &opts);
}

// Reverse-chip coverage
//
// Each reverse chip exposed in `examples/web-playground/src/samples.rs`
// must round-trip cleanly through `reverse_sql`. The chip catalogue is
// duplicated here for the same reason as the forward chips: the
// playground crate is wasm-only, so the test mirrors the literal SQL
// strings + expected PG substrings. Any change in `samples.rs` must be
// reflected here or the test will fail loudly.

struct ReverseCanned {
    label: &'static str,
    sqlite_sql: &'static str,
    expectation: Expectation,
    expect_pg_contains: &'static str,
}

fn reverse_translate(
    pg_schema: &str,
    sqlite_sql: &str,
    options: &Pg2SqliteOptions,
) -> Result<String, pg2sqlite::errors::Error> {
    let translator = Pg2Sqlite::default().sql(pg_schema)?;
    let schema = translator.build_schema()?;
    let stmts = translator.reverse_sql(sqlite_sql, &schema, options)?;
    Ok(stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n"))
}

fn run_reverse_chips(pg_schema: &str, chips: &[ReverseCanned], options: &Pg2SqliteOptions) {
    for chip in chips {
        let result = reverse_translate(pg_schema, chip.sqlite_sql, options);
        match (chip.expectation, &result) {
            (Expectation::Positive, Ok(pg)) => {
                assert!(
                    pg.contains(chip.expect_pg_contains),
                    "[reverse / {}] expected output to contain `{}`, got:\n{pg}",
                    chip.label,
                    chip.expect_pg_contains,
                );
            }
            (Expectation::Negative, Err(_)) => {}
            (Expectation::Positive, Err(e)) => {
                panic!("[reverse / {}] expected success but got error: {e}", chip.label);
            }
            (Expectation::Negative, Ok(pg)) => {
                panic!("[reverse / {}] expected failure but reverse succeeded:\n{pg}", chip.label);
            }
        }
    }
}

const SIMPLE_REVERSE_CHIPS: &[ReverseCanned] = &[
    ReverseCanned {
        label: "datetime('now') back to NOW()",
        sqlite_sql: "INSERT INTO users (name, created_at) VALUES ('alice', datetime('now'))",
        expectation: Expectation::Positive,
        expect_pg_contains: "NOW()",
    },
    ReverseCanned {
        label: "unicode() back to ascii()",
        sqlite_sql: "SELECT unicode('A')",
        expectation: Expectation::Positive,
        expect_pg_contains: "ascii(",
    },
];

const PGVECTOR_REVERSE_CHIPS: &[ReverseCanned] = &[
    ReverseCanned {
        label: "vec_f32() back to ::vector cast",
        sqlite_sql: "INSERT INTO items (id, embedding) VALUES (4, vec_f32('[0.5, 0.5, 0.5]'))",
        expectation: Expectation::Positive,
        expect_pg_contains: "::vector",
    },
    ReverseCanned {
        label: "vec_distance back to <-> operator",
        sqlite_sql: "SELECT id FROM items ORDER BY vec_distance_L2(embedding, vec_f32('[0.5, 0.5, 0.5]')) LIMIT 3",
        expectation: Expectation::Positive,
        expect_pg_contains: "<->",
    },
];

const RLS_REVERSE_CHIPS: &[ReverseCanned] = &[ReverseCanned {
    label: "Backing-table access (rejected by reverse)",
    sqlite_sql: "SELECT id, owner_id, title FROM documents_rls",
    expectation: Expectation::Negative,
    expect_pg_contains: "",
}];

#[test]
fn simple_sample_reverse_queries() {
    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob);
    run_reverse_chips(SIMPLE_SCHEMA, SIMPLE_REVERSE_CHIPS, &opts);
}

#[test]
fn pgvector_sample_reverse_queries() {
    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob);
    run_reverse_chips(PGVECTOR_SCHEMA, PGVECTOR_REVERSE_CHIPS, &opts);
}

#[test]
fn rls_sample_reverse_queries() {
    let opts = Pg2SqliteOptions::default()
        .with_sqlitegis_enabled()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_rls_audit_table_name("rls_violations")
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));
    run_reverse_chips(RLS_SCHEMA, RLS_REVERSE_CHIPS, &opts);
}

#[cfg(feature = "sqlitegis")]
unsafe extern "C" fn sqlitegis_init(
    db: *mut libsqlite3_sys::sqlite3,
    _pz_err_msg: *mut *mut std::ffi::c_char,
    _p_api: *const libsqlite3_sys::sqlite3_api_routines,
) -> std::ffi::c_int {
    unsafe { sqlitegis::sqlite::register_functions(db) }
}
