//! Curated sample schemas surfaced in the Step 1 dropdown.
//!
//! Each `Sample` carries an inline SQL string + an option-preset function
//! that the picker invokes on the live `Pg2SqliteOptions` signal so the
//! sample's translation has everything it needs to succeed without
//! requiring the user to open the Advanced options form first.

use pg2sqlite::prelude::SessionVariableMapping;

use crate::state::{QueryDialect, WebOptions};

/// A picker entry. `name` is what the user sees on the badge; `sql`
/// is what fills the editor when clicked; `apply_options` is run
/// against the current options state so RLS / PostGIS / UUID samples
/// pre-configure their dependencies. `queries` is the curated set
/// of follow-up SQL the query panel renders as chips while the
/// editor still holds an unedited copy of `sql`.
pub struct Sample {
    pub name: &'static str,
    pub sql: &'static str,
    pub apply_options: fn(&mut WebOptions),
    pub queries: &'static [SampleQuery],
}

/// A single canned follow-up query attached to a sample.
pub struct SampleQuery {
    /// Short label shown on the chip.
    pub label: &'static str,
    /// SQL loaded into the query editor and run on click.
    pub sql: &'static str,
    /// Which dialect the query is authored in. Postgres input goes
    /// through pg2sqlite's forward translator before it hits the
    /// in-memory connection; SQLite input is run verbatim. FTS5
    /// `MATCH`, raw `vec0`/`docs_fts` access, and other SQLite-only
    /// syntax must use `Sqlite` so they bypass the PG parser.
    pub dialect: QueryDialect,
    /// Whether running this query is expected to succeed or to be
    /// blocked by a policy / constraint / generated column. Drives
    /// the chip's styling so negative demos are visually distinct.
    pub kind: SampleQueryKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SampleQueryKind {
    Positive,
    Negative,
}

/// Look up a sample by its `name`. Used by the badge component to
/// avoid passing `Sample` itself as a Dioxus prop - Dioxus props
/// need PartialEq, and deriving PartialEq on a struct with an `fn`
/// field triggers `unpredictable_function_pointer_comparisons`.
/// Names are unique within `SAMPLES`, so the lookup is exact.
#[must_use]
pub fn find_sample(name: &str) -> Option<&'static Sample> {
    SAMPLES.iter().find(|s| s.name == name)
}

/// Look up a sample by exact source-SQL match. Used by the query
/// panel to decide whether to render canned-query chips: as soon as
/// the user edits the PG input, the match breaks and the chips
/// disappear (since the curated queries no longer fit the schema).
#[must_use]
pub fn find_sample_by_sql(pg_sql: &str) -> Option<&'static Sample> {
    SAMPLES.iter().find(|s| s.sql == pg_sql)
}

const SIMPLE_SQL: &str = "\
-- A minimal PostgreSQL schema. Translates without any extra options.
CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT NOW()
);

INSERT INTO users (name) VALUES ('alice'), ('bob'), ('carol');
";

const FTS5_SQL: &str = "\
-- PostgreSQL full-text search via GIN over a tsvector expression
-- becomes a SQLite FTS5 virtual table with sync triggers.
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

const PGVECTOR_SQL: &str = "\
-- pgvector columns and distance operators map to sqlite-vec.
-- Note: the playground does not load sqlite-vec at runtime (yet),
-- so queries using vec_distance_* will translate but fail to execute.
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

const POSTGIS_SQL: &str = "\
-- PostGIS geometry columns become BLOBs; GiST indexes become
-- `SELECT CreateSpatialIndex(...)`. Requires SQLiteGIS at runtime.
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

const RLS_SQL: &str = "\
-- Row-Level Security: PostgreSQL `CREATE POLICY` translates to a
-- renamed backing table + a filtered view + INSTEAD OF triggers.
-- The session-variable mapping below is auto-configured when this
-- sample is picked, and the playground registers a no-arg SQLite
-- UDF `current_app_user()` that returns 42 so the policies have a
-- concrete principal to evaluate against.
--
-- Only rows owned by user 42 are seeded here, since any insert with
-- a different `owner_id` would fail the WITH CHECK policy at apply
-- time (just like in real Postgres with RLS enabled). Try running
-- this in the query panel below once apply succeeds to see the
-- policy fire:
--
--   INSERT INTO documents (id, owner_id, title)
--   VALUES (99, 99, 'Owned by someone else');
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

const CONSTRAINTS_SQL: &str = "\
-- CHECK constraints, NUMERIC, DEFAULT NOW() and GENERATED ALWAYS AS
-- (...) STORED all translate one-to-one into SQLite. The total
-- column is computed on every write and treated as read-only by
-- SQLite, so any attempt to INSERT or UPDATE it directly is
-- rejected with `cannot INSERT into generated column \"total\"`.
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

const SIMPLE_QUERIES: &[SampleQuery] = &[
    SampleQuery {
        label: "All users",
        sql: "SELECT * FROM users",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
    SampleQuery {
        label: "Find alice",
        sql: "SELECT * FROM users WHERE name = 'alice'",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
    SampleQuery {
        label: "Add a new user",
        sql: "INSERT INTO users (name) VALUES ('dave')",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
];

// pg2sqlite translates `to_tsvector(...) @@ to_tsquery(...)` into a
// SQLite FTS5 `... IN (SELECT rowid FROM docs_fts WHERE docs_fts
// MATCH '...')` subquery, so the chip queries are authored in PG
// syntax and let the translator rewrite them at click time.
const FTS5_QUERIES: &[SampleQuery] = &[
    SampleQuery {
        label: "All docs",
        sql: "SELECT id, body FROM docs",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
    SampleQuery {
        label: "Match 'pg2sqlite'",
        sql: "SELECT id, body FROM docs WHERE to_tsvector('english', body) @@ to_tsquery('pg2sqlite')",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
    SampleQuery {
        label: "Match 'sqlite'",
        sql: "SELECT id, body FROM docs WHERE to_tsvector('english', body) @@ to_tsquery('sqlite')",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
];

const PGVECTOR_QUERIES: &[SampleQuery] = &[
    SampleQuery {
        label: "All items",
        sql: "SELECT id, embedding FROM items",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
    SampleQuery {
        label: "Nearest to [0.5, 0.5, 0.5]",
        sql: "SELECT id FROM items ORDER BY embedding <-> '[0.5, 0.5, 0.5]' LIMIT 3",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
    SampleQuery {
        label: "Insert a vector",
        // Omit `id` so SQLite auto-assigns the integer primary key.
        // A hardcoded id would trip the UNIQUE constraint the second
        // time the user clicks this chip; the auto-assigned rowid
        // makes the chip re-runnable.
        sql: "INSERT INTO items (embedding) VALUES ('[0.5, 0.5, 0.5]')",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
];

const POSTGIS_QUERIES: &[SampleQuery] = &[
    SampleQuery {
        label: "All places (lon / lat)",
        sql: "SELECT id, name, ST_X(geom) AS lon, ST_Y(geom) AS lat FROM places",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
    SampleQuery {
        label: "Inside Europe bbox",
        sql: "SELECT name FROM places WHERE ST_Within(geom, ST_MakeEnvelope(-10, 35, 30, 70))",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
    SampleQuery {
        label: "Find Helsinki",
        sql: "SELECT name, ST_X(geom) AS lon, ST_Y(geom) AS lat FROM places WHERE name = 'Helsinki'",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
];

const RLS_QUERIES: &[SampleQuery] = &[
    SampleQuery {
        label: "My visible documents",
        sql: "SELECT * FROM documents",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
    SampleQuery {
        label: "Insert my own row",
        // Omit `id` so SQLite auto-assigns the integer primary key,
        // letting the user click this chip repeatedly without
        // tripping a UNIQUE-constraint failure on the seeded rows.
        sql: "INSERT INTO documents (owner_id, title) VALUES (42, 'Third draft')",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
    SampleQuery {
        label: "Insert for another user (blocked)",
        sql: "INSERT INTO documents (id, owner_id, title) VALUES (99, 99, 'Owned by someone else')",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Negative,
    },
];

const CONSTRAINTS_QUERIES: &[SampleQuery] = &[
    SampleQuery {
        label: "All orders (with computed total)",
        sql: "SELECT id, quantity, unit_price, total FROM orders",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
    SampleQuery {
        label: "Add a valid order",
        sql: "INSERT INTO orders (quantity, unit_price) VALUES (5, 12.50)",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Positive,
    },
    SampleQuery {
        label: "Write to generated total (blocked)",
        sql: "INSERT INTO orders (quantity, unit_price, total) VALUES (1, 1, 999)",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Negative,
    },
    SampleQuery {
        label: "Negative quantity (CHECK blocks)",
        sql: "INSERT INTO orders (quantity, unit_price) VALUES (-1, 10)",
        dialect: QueryDialect::Postgres,
        kind: SampleQueryKind::Negative,
    },
];

/// All samples, in dropdown order.
pub const SAMPLES: &[Sample] = &[
    Sample {
        name: "Simple",
        sql: SIMPLE_SQL,
        apply_options: simple_options,
        queries: SIMPLE_QUERIES,
    },
    Sample { name: "FTS5", sql: FTS5_SQL, apply_options: simple_options, queries: FTS5_QUERIES },
    Sample {
        name: "pgvector",
        sql: PGVECTOR_SQL,
        apply_options: simple_options,
        queries: PGVECTOR_QUERIES,
    },
    Sample {
        name: "PostGIS",
        sql: POSTGIS_SQL,
        apply_options: postgis_options,
        queries: POSTGIS_QUERIES,
    },
    Sample { name: "RLS", sql: RLS_SQL, apply_options: rls_options, queries: RLS_QUERIES },
    Sample {
        name: "Constraints",
        sql: CONSTRAINTS_SQL,
        apply_options: simple_options,
        queries: CONSTRAINTS_QUERIES,
    },
];

fn simple_options(opts: &mut WebOptions) {
    *opts = WebOptions::default();
}

fn postgis_options(opts: &mut WebOptions) {
    *opts = WebOptions::default();
}

fn rls_options(opts: &mut WebOptions) {
    // The `current_setting('app.user_id')` references in the policies need
    // to be rewritten into a SQLite function call. We map them to
    // `current_app_user()`, which the user is expected to provide as a
    // UDF in real deployments; for the playground demo it would be a no-op
    // until query execution lands.
    *opts = WebOptions {
        rls_audit_table_name: "rls_violations".to_string(),
        session_variables: vec![SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        )],
        ..WebOptions::default()
    };
}
