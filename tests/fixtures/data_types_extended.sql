-- Extended data type tests: JSON, JSONB, complex defaults

CREATE TABLE json_data (
    id SERIAL PRIMARY KEY,
    metadata JSON,
    settings JSONB NOT NULL DEFAULT '{}'
);

CREATE TABLE complex_defaults (
    id SERIAL PRIMARY KEY,
    negative_value INTEGER NOT NULL DEFAULT -1,
    calculated REAL NOT NULL DEFAULT 3.14,
    bool_default BOOLEAN NOT NULL DEFAULT true,
    text_default TEXT DEFAULT 'hello'
);

CREATE TABLE upsert_test (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    counter INTEGER NOT NULL DEFAULT 0
);
