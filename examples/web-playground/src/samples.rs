//! Curated sample schemas surfaced in the Step 1 dropdown.

use pg2sqlite::prelude::SessionVariableMapping;

use crate::state::{QueryDialect, WebOptions};

/// A picker entry.
pub struct Sample {
    pub name: &'static str,
    /// An enum so renaming a badge cannot silently drop its icon.
    pub icon: SampleIcon,
    pub sql: &'static str,
    pub apply_options: fn(&mut WebOptions),
    pub queries: &'static [SampleQuery],
    pub reverse_queries: &'static [ReverseSampleQuery],
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SampleIcon {
    Table,
    FullText,
    Vector,
    Geometry,
    Policy,
    Constraint,
}

pub struct SampleQuery {
    pub label: &'static str,
    /// SQL run verbatim if `dialect` is `Sqlite`, or forward-translated if
    /// `Postgres`.
    pub sql: &'static str,
    pub dialect: QueryDialect,
    /// Drives chip styling: negative chips show expected-failure demos.
    pub kind: SampleQueryKind,
}

/// A canned SQLite snippet the reverse panel renders as a chip.
pub struct ReverseSampleQuery {
    pub label: &'static str,
    pub sql: &'static str,
    pub kind: SampleQueryKind,
    /// Substring that must appear in the reverse-translated PG SQL. Consumed by
    /// `test_playground_sample_queries` in the parent repo, not the UI. The
    /// catalogue here and that test file must be kept in sync.
    #[allow(dead_code)]
    pub expect_pg_contains: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SampleQueryKind {
    Positive,
    Negative,
}

/// Looks up by name because deriving `PartialEq` on a struct with an `fn` field
/// trips `unpredictable_function_pointer_comparisons`. Names are unique within
/// `SAMPLES`.
#[must_use]
pub fn find_sample(name: &str) -> Option<&'static Sample> {
    SAMPLES.iter().find(|s| s.name == name)
}

/// Looks up by exact source SQL to decide whether to render canned-query chips.
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
    unit_price NUMERIC(10,2) NOT NULL CHECK (unit_price >= 0),
    total NUMERIC(12,2) GENERATED ALWAYS AS (quantity * unit_price) STORED,
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
        // time the user clicks this chip. The auto-assigned rowid keeps
        // the chip re-runnable.
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

// Reverse chips only for samples where SQLite and PG syntax meaningfully
// differ.

const SIMPLE_REVERSE_QUERIES: &[ReverseSampleQuery] = &[
    ReverseSampleQuery {
        label: "datetime('now') back to NOW()",
        sql: "INSERT INTO users (name, created_at) VALUES ('alice', datetime('now'))",
        kind: SampleQueryKind::Positive,
        expect_pg_contains: "NOW()",
    },
    ReverseSampleQuery {
        label: "unicode() back to ascii()",
        sql: "SELECT unicode('A')",
        kind: SampleQueryKind::Positive,
        expect_pg_contains: "ascii(",
    },
];

const PGVECTOR_REVERSE_QUERIES: &[ReverseSampleQuery] = &[
    ReverseSampleQuery {
        label: "vec_f32() back to ::vector cast",
        sql: "INSERT INTO items (id, embedding) VALUES (4, vec_f32('[0.5, 0.5, 0.5]'))",
        kind: SampleQueryKind::Positive,
        expect_pg_contains: "::vector",
    },
    ReverseSampleQuery {
        label: "vec_distance back to <-> operator",
        sql: "SELECT id FROM items ORDER BY vec_distance_L2(embedding, vec_f32('[0.5, 0.5, 0.5]')) LIMIT 3",
        kind: SampleQueryKind::Positive,
        expect_pg_contains: "<->",
    },
];

const RLS_REVERSE_QUERIES: &[ReverseSampleQuery] = &[
    // Negative chip: the reverse translator rejects `_rls`-suffixed backing-table names.
    ReverseSampleQuery {
        label: "Backing-table access (rejected by reverse)",
        sql: "SELECT id, owner_id, title FROM documents_rls",
        kind: SampleQueryKind::Negative,
        expect_pg_contains: "",
    },
];

/// All samples, in dropdown order. Each `name` pairs the PG construct with the
/// SQLite construct.
pub const SAMPLES: &[Sample] = &[
    Sample {
        name: "SERIAL → INTEGER",
        icon: SampleIcon::Table,
        sql: SIMPLE_SQL,
        apply_options: simple_options,
        queries: SIMPLE_QUERIES,
        reverse_queries: SIMPLE_REVERSE_QUERIES,
    },
    Sample {
        name: "GIN → FTS5",
        icon: SampleIcon::FullText,
        sql: FTS5_SQL,
        apply_options: simple_options,
        queries: FTS5_QUERIES,
        reverse_queries: &[],
    },
    Sample {
        name: "pgvector → sqlite-vec",
        icon: SampleIcon::Vector,
        sql: PGVECTOR_SQL,
        apply_options: simple_options,
        queries: PGVECTOR_QUERIES,
        reverse_queries: PGVECTOR_REVERSE_QUERIES,
    },
    Sample {
        name: "PostGIS → SQLiteGIS",
        icon: SampleIcon::Geometry,
        sql: POSTGIS_SQL,
        apply_options: postgis_options,
        queries: POSTGIS_QUERIES,
        reverse_queries: &[],
    },
    Sample {
        name: "RLS → INSTEAD OF",
        icon: SampleIcon::Policy,
        sql: RLS_SQL,
        apply_options: rls_options,
        queries: RLS_QUERIES,
        reverse_queries: RLS_REVERSE_QUERIES,
    },
    Sample {
        name: "NUMERIC → REAL",
        icon: SampleIcon::Constraint,
        sql: CONSTRAINTS_SQL,
        apply_options: simple_options,
        queries: CONSTRAINTS_QUERIES,
        reverse_queries: &[],
    },
];

fn simple_options(opts: &mut WebOptions) {
    *opts = WebOptions::default();
}

fn postgis_options(opts: &mut WebOptions) {
    *opts = WebOptions::default();
}

fn rls_options(opts: &mut WebOptions) {
    // Maps `current_setting('app.user_id')` to a SQLite UDF call
    // `current_app_user()`.
    *opts = WebOptions {
        rls_audit_table_name: "rls_violations".to_string(),
        session_variables: vec![SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        )],
        ..WebOptions::default()
    };
}
