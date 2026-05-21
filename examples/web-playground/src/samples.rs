//! Curated sample schemas surfaced in the Step 1 dropdown.
//!
//! Each `Sample` carries an inline SQL string + an option-preset function
//! that the picker invokes on the live `Pg2SqliteOptions` signal so the
//! sample's translation has everything it needs to succeed without
//! requiring the user to open the Advanced options form first.

use pg2sqlite::prelude::{SessionVariableMapping, UuidRepresentation};

use crate::state::WebOptions;

/// A picker entry. `name` is what the user sees on the badge; `sql`
/// is what fills the editor when clicked; `apply_options` is run
/// against the current options state so RLS / PostGIS / UUID samples
/// pre-configure their dependencies.
pub struct Sample {
    pub name: &'static str,
    pub sql: &'static str,
    pub apply_options: fn(&mut WebOptions),
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
-- `SELECT CreateSpatialIndex(...)`. Requires geolite at runtime.
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
-- sample is picked.
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
    (2, 42, 'Second draft'),
    (3, 99, 'Other user document');
";

/// All samples, in dropdown order.
pub const SAMPLES: &[Sample] = &[
    Sample { name: "Simple", sql: SIMPLE_SQL, apply_options: simple_options },
    Sample { name: "FTS5", sql: FTS5_SQL, apply_options: simple_options },
    Sample { name: "pgvector", sql: PGVECTOR_SQL, apply_options: simple_options },
    Sample { name: "PostGIS", sql: POSTGIS_SQL, apply_options: postgis_options },
    Sample { name: "RLS", sql: RLS_SQL, apply_options: rls_options },
];

fn simple_options(opts: &mut WebOptions) {
    *opts = WebOptions::default();
}

fn postgis_options(opts: &mut WebOptions) {
    *opts = WebOptions { geolite_enabled: true, ..WebOptions::default() };
}

fn rls_options(opts: &mut WebOptions) {
    // The `current_setting('app.user_id')` references in the policies need
    // to be rewritten into a SQLite function call. We map them to
    // `current_app_user()`, which the user is expected to provide as a
    // UDF in real deployments; for the playground demo it would be a no-op
    // until query execution lands.
    *opts = WebOptions {
        uuid_representation: Some(UuidRepresentation::Blob),
        rls_audit_table_name: "rls_violations".to_string(),
        session_variables: vec![SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        )],
        ..WebOptions::default()
    };
}
