// The validity-sweep corpus, shared between the sweep test and the floor
// emitter.
//
// `tests/test_emitted_sql_is_valid_sqlite.rs` executes every row against the
// bundled SQLite (the crate's central guarantee), and
// `examples/floor_corpus.rs` emits every row's translation for
// `scripts/check_sqlite_floor.sh`, so a corpus row is automatically a floor
// check. One home for the data keeps the two proofs over the same corpus.

use pg2sqlite::prelude::{
    ArrayRepresentation, Pg2SqliteOptions, TranslationOptions, UuidRepresentation,
};

/// Declared up front so no case fails merely for naming an unknown relation.
pub const FIXTURE: &str = "
CREATE TABLE t (
    id INT PRIMARY KEY,
    n INT,
    r REAL,
    s TEXT,
    b BOOLEAN,
    ts TIMESTAMP,
    d DATE,
    payload JSONB,
    tags TEXT[],
    blob BYTEA
);
CREATE TABLE u (id INT PRIMARY KEY, t_id INT, s TEXT);
";

/// Every capability switched on, so a construct is never rejected merely for
/// lacking an opt-in. Declaring the nine statistical aggregates is one such
/// opt-in: SQLite has none of them, so the translator refuses each until the
/// caller says the destination carries it. The sweep's connection registers
/// exactly these, and `the_sweep_declares_every_registered_aggregate` holds
/// the two lists together.
#[must_use]
pub fn sweep_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_array_representation(ArrayRepresentation::Json)
        .with_math_functions_available()
        .with_rls_audit_table_name("rls_violations")
        .with_uuid_v7_function_name("uuid7")
        .with_user_defined_functions([
            "var_pop",
            "var_samp",
            "variance",
            "stddev_pop",
            "stddev",
            "stddev_samp",
            "covar_pop",
            "covar_samp",
            "corr",
        ])
}

/// What a corpus row's translation adds on top of the translated fixture.
///
/// The fixture is identified by value rather than by position, because a row
/// can make the translator emit a statement the fixture alone does not: a row
/// holding a LIKE gets the case-sensitivity pragma prepended, and counting
/// then drops the pragma and re-creates a fixture table instead.
#[must_use]
pub fn row_statements(setup: &[String], all: &[String]) -> Vec<String> {
    all.iter().filter(|statement| !setup.contains(statement)).cloned().collect()
}

/// Every corpus group: `(label, cases)`.
pub const CORPUS_GROUPS: &[(&str, &[&str])] = &[
    (
        "dml",
        &[
            "INSERT INTO t (id) VALUES (1), (2), (3);",
            "INSERT INTO t DEFAULT VALUES;",
            "INSERT INTO t (id) VALUES (1) ON CONFLICT DO NOTHING;",
            "INSERT INTO t (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET n = 1;",
            "INSERT INTO t (id) VALUES (1) RETURNING id;",
            "INSERT INTO t (id, n) SELECT id, t_id FROM u;",
            "UPDATE t SET n = 1 WHERE id = 1;",
            "UPDATE t SET n = 1, s = 'x' WHERE id = 1;",
            "UPDATE t SET (n, s) = (1, 'x') WHERE id = 1;",
            "UPDATE t SET n = u.id FROM u WHERE u.t_id = t.id;",
            "UPDATE t SET n = 1 RETURNING id;",
            "DELETE FROM t WHERE id = 1;",
            "DELETE FROM t USING u WHERE u.t_id = t.id;",
            "DELETE FROM t RETURNING id;",
            "MERGE INTO t USING u ON t.id = u.t_id WHEN MATCHED THEN UPDATE SET n = 1;",
            "TRUNCATE t;",
                ],
    ),
    (
        "query",
        &[
            "SELECT * FROM t;",
            "SELECT DISTINCT n FROM t;",
            "SELECT DISTINCT ON (n) n, s FROM t ORDER BY n;",
            "SELECT n, count(*) FROM t GROUP BY n HAVING count(*) > 1;",
            "SELECT n FROM t ORDER BY n NULLS FIRST;",
            "SELECT n FROM t ORDER BY n DESC NULLS LAST;",
            "SELECT n FROM t LIMIT 1 OFFSET 2;",
            "SELECT n FROM t OFFSET 2 ROWS FETCH FIRST 3 ROWS ONLY;",
            "SELECT n FROM t FETCH FIRST 3 ROWS ONLY;",
            "SELECT n FROM t FOR UPDATE;",
            "SELECT n FROM t FOR SHARE;",
            "SELECT * FROM t JOIN u ON t.id = u.t_id;",
            "SELECT * FROM t LEFT JOIN u ON t.id = u.t_id;",
            "SELECT * FROM t RIGHT JOIN u ON t.id = u.t_id;",
            "SELECT * FROM t FULL OUTER JOIN u ON t.id = u.t_id;",
            "SELECT * FROM t CROSS JOIN u;",
            "SELECT * FROM t NATURAL JOIN u;",
            "SELECT * FROM t, LATERAL (SELECT 1) AS x;",
            "SELECT * FROM t, LATERAL (SELECT t.id) AS x;",
            "WITH c AS (SELECT 1 AS a) SELECT a FROM c;",
            "WITH RECURSIVE c(a) AS (SELECT 1 UNION ALL SELECT a + 1 FROM c WHERE a < 5) SELECT a FROM c;",
            "WITH c AS MATERIALIZED (SELECT 1 AS a) SELECT a FROM c;",
            "SELECT 1 UNION SELECT 2;",
            "SELECT 1 UNION ALL SELECT 2;",
            "SELECT 1 INTERSECT SELECT 2;",
            "SELECT 1 EXCEPT SELECT 2;",
            "SELECT n FROM t GROUP BY GROUPING SETS ((n), ());",
            "SELECT n FROM t GROUP BY ROLLUP (n);",
            "SELECT n FROM t GROUP BY CUBE (n);",
            "SELECT * FROM (VALUES (1), (2)) AS v(a);",
            "SELECT * FROM t TABLESAMPLE BERNOULLI (10);",
                ],
    ),
    (
        "window",
        &[
            "SELECT row_number() OVER (ORDER BY n) FROM t;",
            "SELECT rank() OVER (PARTITION BY n ORDER BY id) FROM t;",
            "SELECT sum(n) OVER (ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM t;",
            "SELECT sum(n) OVER (RANGE BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) FROM t;",
            "SELECT sum(n) OVER (GROUPS BETWEEN 1 PRECEDING AND 1 FOLLOWING) FROM t;",
            "SELECT sum(n) OVER (ORDER BY n EXCLUDE CURRENT ROW) FROM t;",
            "SELECT sum(n) OVER w FROM t WINDOW w AS (ORDER BY n);",
            "SELECT lag(n, 1, 0) OVER (ORDER BY id) FROM t;",
            "SELECT first_value(n) OVER (ORDER BY id) FROM t;",
            "SELECT nth_value(n, 2) OVER (ORDER BY id) FROM t;",
            "SELECT ntile(4) OVER (ORDER BY id) FROM t;",
            "SELECT percent_rank() OVER (ORDER BY id) FROM t;",
            "SELECT cume_dist() OVER (ORDER BY id) FROM t;",
                ],
    ),
    (
        "operator",
        &[
            "SELECT n + 1, n - 1, n * 2, n / 2, n % 2 FROM t;",
            "SELECT n ^ 2 FROM t;",
            "SELECT |/ r, ||/ r FROM t;",
            "SELECT @ n FROM t;",
            "SELECT n & 1, n | 1, ~n, n << 1, n >> 1 FROM t;",
            "SELECT n # 1 FROM t;",
            "SELECT s || 'x' FROM t;",
            "SELECT s ~ 'a' FROM t;",
            "SELECT s ~* 'a' FROM t;",
            "SELECT s !~ 'a' FROM t;",
            "SELECT s !~* 'a' FROM t;",
            "SELECT s LIKE 'a%', s NOT LIKE 'a%', s ILIKE 'a%' FROM t;",
            "SELECT s SIMILAR TO 'a%' FROM t;",
            "SELECT s LIKE 'a%' ESCAPE '!' FROM t;",
            "SELECT b IS TRUE, b IS NOT TRUE, b IS FALSE, b IS UNKNOWN FROM t;",
            "SELECT n IS NULL, n IS NOT NULL FROM t;",
            "SELECT n IS DISTINCT FROM 1, n IS NOT DISTINCT FROM 1 FROM t;",
            "SELECT n BETWEEN 1 AND 2, n NOT BETWEEN 1 AND 2 FROM t;",
            "SELECT n IN (1, 2), n NOT IN (1, 2) FROM t;",
            "SELECT (n, s) = (1, 'x') FROM t;",
            "SELECT ROW(n, s) FROM t;",
                ],
    ),
    (
        "json",
        &[
            "SELECT payload -> 'a', payload ->> 'a' FROM t;",
            "SELECT payload #> '{a,b}', payload #>> '{a,b}' FROM t;",
            "SELECT payload @> '{}' FROM t;",
            "SELECT payload <@ '{}' FROM t;",
            "SELECT payload ? 'a' FROM t;",
            "SELECT payload ?| ARRAY['a', 'b'] FROM t;",
            "SELECT payload ?& ARRAY['a', 'b'] FROM t;",
            "SELECT payload #- '{a}' FROM t;",
            "SELECT n IS JSON, s IS JSON ARRAY FROM t;",
                ],
    ),
    (
        "expr",
        &[
            "SELECT CASE WHEN n > 0 THEN 'p' ELSE 'n' END FROM t;",
            "SELECT CASE n WHEN 1 THEN 'a' END FROM t;",
            "SELECT COALESCE(n, 0), NULLIF(n, 0), GREATEST(n, 1), LEAST(n, 1) FROM t;",
            "SELECT EXISTS (SELECT 1 FROM u), NOT EXISTS (SELECT 1 FROM u);",
            "SELECT (SELECT max(id) FROM u);",
            "SELECT n::text, n::bigint, r::numeric FROM t;",
            "SELECT CAST(n AS TEXT) FROM t;",
            "SELECT s COLLATE \"C\" FROM t;",
            "SELECT INTERVAL '1 day';",
            "SELECT ts AT TIME ZONE 'UTC' FROM t;",
            "SELECT EXTRACT(YEAR FROM ts) FROM t;",
            "SELECT ARRAY[1, 2, 3];",
            "SELECT tags[1] FROM t;",
                ],
    ),
    (
        "func",
        &[
            "SELECT length(s), upper(s), lower(s), trim(s), btrim(s) FROM t;",
            "SELECT substring(s FROM 1 FOR 2), substr(s, 1, 2) FROM t;",
            "SELECT position('a' IN s), strpos(s, 'a') FROM t;",
            "SELECT replace(s, 'a', 'b') FROM t;",
            "SELECT abs(n), ceil(r), floor(r), round(r), round(r, 2), trunc(r) FROM t;",
            "SELECT mod(n, 2), div(n, 2) FROM t;",
            "SELECT random();",
            "SELECT now(), current_date, current_time, current_timestamp, localtimestamp;",
            "SELECT date_trunc('day', ts) FROM t;",
            "SELECT date_part('year', ts) FROM t;",
            "SELECT to_char(ts, 'YYYY-MM-DD') FROM t;",
            "SELECT to_timestamp(0);",
            "SELECT count(*), count(n), sum(n), avg(n), min(n), max(n) FROM t;",
            "SELECT count(DISTINCT n) FROM t;",
            "SELECT count(*) FILTER (WHERE n > 0) FROM t;",
            "SELECT string_agg(s, ',') FROM t;",
            "SELECT string_agg(s, ',' ORDER BY s) FROM t;",
            "SELECT json_agg(s), jsonb_agg(s) FROM t;",
            "SELECT json_build_object('a', n), json_build_array(n) FROM t;",
            "SELECT jsonb_set(payload, '{a}', '1') FROM t;",
            "SELECT json_extract_path(payload, 'a') FROM t;",
            "SELECT var_pop(r), var_samp(r), variance(r) FROM t;",
            "SELECT stddev(r), stddev_pop(r), stddev_samp(r) FROM t;",
            "SELECT covar_pop(r, r), covar_samp(r, r), corr(r, r) FROM t;",
            // Names the translator does not recognise. The sweep counts
            // `no such` as a translator fault, so these would have caught the
            // passthrough that emitted them verbatim, had the corpus ever
            // carried one. A rejection is the valid outcome.
            "SELECT pg_sleep(1);",
            "SELECT definitely_not_a_function(n) FROM t;",
            "SELECT ST_Point(0, 0);",
                ],
    ),
    (
        "ddl",
        &[
            "CREATE TABLE a (id INT GENERATED ALWAYS AS IDENTITY PRIMARY KEY);",
            "CREATE TABLE a (id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY);",
            "CREATE TABLE a (id INT PRIMARY KEY, x INT GENERATED ALWAYS AS (id * 2) STORED);",
            "CREATE TABLE a (id INT PRIMARY KEY, x INT CHECK (x > 0));",
            "CREATE TABLE a (id INT PRIMARY KEY, x INT UNIQUE);",
            "CREATE TABLE a (id INT PRIMARY KEY, x INT REFERENCES t(id) ON DELETE CASCADE);",
            "CREATE TABLE a (id INT, x INT, PRIMARY KEY (id, x));",
            "CREATE TABLE a (LIKE t);",
            "CREATE TABLE a () INHERITS (t);",
            "CREATE TABLE a AS SELECT * FROM t;",
            "CREATE UNLOGGED TABLE a (id INT PRIMARY KEY);",
            "CREATE TEMPORARY TABLE a (id INT PRIMARY KEY);",
            "CREATE TABLE IF NOT EXISTS a (id INT PRIMARY KEY);",
            "CREATE INDEX i ON t (n);",
            "CREATE UNIQUE INDEX i ON t (n);",
            "CREATE INDEX i ON t (n DESC NULLS LAST);",
            "CREATE INDEX i ON t (n) WHERE n > 0;",
            "CREATE INDEX i ON t (lower(s));",
            "CREATE INDEX i ON t USING btree (n);",
            "CREATE INDEX i ON t USING hash (n);",
            "CREATE INDEX CONCURRENTLY i ON t (n);",
            "CREATE VIEW v AS SELECT id FROM t;",
            "CREATE OR REPLACE VIEW v AS SELECT id FROM t;",
            "ALTER TABLE t ADD COLUMN x INT;",
            "ALTER TABLE t RENAME COLUMN n TO m;",
            "ALTER TABLE t RENAME TO t2;",
            "ALTER TABLE t ADD CONSTRAINT c CHECK (n > 0);",
            "DROP TABLE t;",
            "DROP TABLE IF EXISTS t CASCADE;",
            "COMMENT ON TABLE t IS 'x';",
                ],
    ),
    (
        "types",
        &[
            "CREATE TABLE a (x SMALLINT, y INTEGER, z BIGINT);",
            "CREATE TABLE a (x DECIMAL(10, 2), y NUMERIC(10, 2));",
            "CREATE TABLE a (x CHAR(3), y VARCHAR(10), z TEXT);",
            "CREATE TABLE a (x DATE, y TIME, z TIMESTAMPTZ);",
            "CREATE TABLE a (x BYTEA, y BIT(8), z BIT VARYING(8));",
            "CREATE TABLE a (x UUID, y JSON, z JSONB);",
            // `y BIGSERIAL` used to sit here too. F6 refuses a serial that is
            // not the primary key, and a refused row emits nothing, so keeping
            // it would have left this row asserting nothing at all.
            "CREATE TABLE a (x SERIAL PRIMARY KEY);",
            "CREATE TABLE a (x INT[], z INT ARRAY[4]);",
            "CREATE TABLE a (x TSVECTOR, y TSQUERY);",
                ],
    ),
    (
        "phase3",
        &[
                // R86: the guarded and ONLY spellings used to pass through.
                "ALTER TABLE IF EXISTS t ADD COLUMN extra TEXT;",
                "ALTER TABLE ONLY t ADD COLUMN extra2 TEXT;",
                // R88: a set-returning function in FROM position.
                "SELECT * FROM json_array_elements('[1,2]');",
                // R90: DISTINCT ON ordered by a column the derived table
                // does not expose.
                "SELECT DISTINCT ON (n) n, s AS latest FROM t ORDER BY n, ts DESC;",
                // R99: a maintenance trigger assignment must translate its
                // value, greatest() being the shape that failed latest.
                "CREATE OR REPLACE FUNCTION set_floor() RETURNS TRIGGER AS $$
                 BEGIN
                     NEW.n := greatest(NEW.n, 1);
                     RETURN NEW;
                 END;
                 $$ LANGUAGE plpgsql;
                 CREATE TRIGGER t_floor BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION set_floor();",
                    ],
    ),
    (
        "numeric",
        &[
                // The D1 writer doors: DEFAULT, VALUES, INSERT SELECT, and
                // UPDATE each scale a literal into minor units.
                "CREATE TABLE nm (id INT PRIMARY KEY, amount NUMERIC(10, 2) DEFAULT 1.50);
                 INSERT INTO nm (id) VALUES (1);",
                "CREATE TABLE nm (id INT PRIMARY KEY, amount NUMERIC(10, 2));
                 INSERT INTO nm (id, amount) VALUES (1, 12.34);",
                "CREATE TABLE nm (id INT PRIMARY KEY, amount NUMERIC(10, 2));
                 INSERT INTO nm (id, amount) SELECT 1, 12.34;",
                "CREATE TABLE nm (id INT PRIMARY KEY, amount NUMERIC(10, 2));
                 UPDATE nm SET amount = 5.75;",
                // R114: the DEC spelling rides the same paths.
                "CREATE TABLE nm (id INT PRIMARY KEY, amount DEC(8, 3));
                 INSERT INTO nm (id, amount) VALUES (1, 1.234);",
                // D1 refusal: bare NUMERIC has no scale to hold.
                "CREATE TABLE nm (id INT PRIMARY KEY, amount NUMERIC);",
                    ],
    ),
    (
        "remediation-ddl",
        &[
                // R102: an ALTER on a protected table lands on the backing
                // table, not the view.
                "CREATE TABLE sec (id INT PRIMARY KEY, extra TEXT);
                 ALTER TABLE sec ENABLE ROW LEVEL SECURITY;
                 CREATE POLICY sec_all ON sec FOR ALL TO PUBLIC USING (true);
                 ALTER TABLE sec ADD COLUMN more TEXT;",
                // R103: foreign CREATE TABLE modifiers stay refusals.
                "CREATE UNLOGGED TABLE ul (id INT PRIMARY KEY);",
                "CREATE TABLE part (id INT PRIMARY KEY) PARTITION BY RANGE (id);",
                // R104: a rename checks the table exists, a guarded ALTER
                // over an undeclared table stays refused.
                "ALTER TABLE t RENAME TO t_renamed;",
                "ALTER TABLE IF EXISTS ghost ADD COLUMN c TEXT;",
                // R105: a column alias list becomes a derived table.
                "SELECT * FROM t AS x (a, b);",
                "SELECT * FROM t AS x (a, b, c, d, e, f, g, h, i, j, k);",
                    ],
    ),
    (
        "trigger",
        &[
                // R117: the all-qualified pg_dump shape, maintenance trigger
                // on an RLS table.
                "CREATE TABLE public.brands (
                     id SERIAL PRIMARY KEY,
                     name VARCHAR(255) NOT NULL,
                     edited_at TEXT
                 );
                 ALTER TABLE public.brands ENABLE ROW LEVEL SECURITY;
                 CREATE POLICY brands_select_all ON public.brands FOR SELECT USING (true);
                 CREATE OR REPLACE FUNCTION update_brands_edited_at() RETURNS TRIGGER AS $$
                 BEGIN
                     NEW.edited_at = CURRENT_TIMESTAMP;
                     RETURN NEW;
                 END;
                 $$ LANGUAGE plpgsql;
                 CREATE TRIGGER trigger_update_brands_edited_at
                 BEFORE UPDATE ON public.brands
                 FOR EACH ROW EXECUTE FUNCTION update_brands_edited_at();",
                // R125: SELECT INTO without FROM, with and without a CTE,
                // bound and consumed inside a trigger body.
                "CREATE TABLE audit3 (id INT PRIMARY KEY, flagged INT);
                 CREATE OR REPLACE FUNCTION flag_fn() RETURNS TRIGGER AS $$
                 DECLARE
                     result INTEGER;
                 BEGIN
                     WITH counts AS (SELECT COUNT(*) AS cnt FROM t)
                     SELECT CASE WHEN (SELECT cnt FROM counts) > 0 THEN 1 ELSE 0 END
                     INTO result;
                     INSERT INTO audit3 (id, flagged) VALUES (NEW.id, result);
                     RETURN NEW;
                 END;
                 $$ LANGUAGE plpgsql;
                 CREATE TRIGGER t_flag BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION flag_fn();",
                "CREATE TABLE audit4 (id INT PRIMARY KEY, flagged INT);
                 CREATE OR REPLACE FUNCTION plain_flag_fn() RETURNS TRIGGER AS $$
                 DECLARE
                     result INTEGER;
                 BEGIN
                     SELECT 1 + 2 INTO result;
                     INSERT INTO audit4 (id, flagged) VALUES (NEW.id, result);
                     RETURN NEW;
                 END;
                 $$ LANGUAGE plpgsql;
                 CREATE TRIGGER t_plain_flag BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION plain_flag_fn();",
                    ],
    ),
    (
        "scalar-srf",
        &[
                // R121: the set-returning JSON family in a SELECT list stays
                // refused, json_each itself included.
                "SELECT jsonb_each(payload) FROM t;",
                "SELECT json_each('{\"a\": 1}');",
                "SELECT jsonb_array_elements(payload) FROM t;",
                    ],
    ),
    (
        "foreign-clause",
        &[
                // R122: the six SELECT clauses foreign to both dialects.
                "SELECT id FROM t LATERAL VIEW now() v AS ts_val;",
                "SELECT id FROM t CLUSTER BY id;",
                "SELECT id FROM t DISTRIBUTE BY id;",
                "SELECT id FROM t SORT BY id;",
                "SELECT id FROM t QUALIFY row_number() OVER (ORDER BY id) = 1;",
                "SELECT id FROM t CONNECT BY id = 2;",
                    ],
    ),
    (
        "window-over",
        &[
                // R120: OVER on a scalar function stays refused.
                "SELECT now() OVER (PARTITION BY n) FROM t;",
                "SELECT to_char(ts, 'YYYY-MM-DD') OVER (PARTITION BY n) FROM t;",
                "SELECT date_trunc('day', ts) OVER (PARTITION BY n) FROM t;",
                "SELECT date_part('year', ts) OVER (PARTITION BY n) FROM t;",
                    ],
    ),
    (
        "rls-delete",
        &[
                // R123: DELETE USING an RLS table reads the policy view.
                "CREATE TABLE rls_users (id INT PRIMARY KEY, active BOOLEAN);
                 ALTER TABLE rls_users ENABLE ROW LEVEL SECURITY;
                 CREATE POLICY ru ON rls_users FOR ALL TO PUBLIC USING (true);
                 CREATE TABLE posts2 (id INT PRIMARY KEY, author INT);
                 DELETE FROM posts2 USING rls_users
                 WHERE posts2.author = rls_users.id AND rls_users.active = false;",
                    ],
    ),
    (
        "quantifier",
        &[
                // R124: the ANY/ALL lowering aliases the item inside the
                // projection, set operations included.
                "SELECT * FROM t WHERE n > ANY (SELECT id FROM u);",
                "SELECT * FROM t WHERE n > ALL (SELECT id FROM u);",
                "SELECT * FROM t WHERE n > ANY (SELECT id FROM u UNION SELECT t_id FROM u);",
                    ],
    ),
    (
        "operators-phase2",
        &[
                // ^@ used to emit verbatim, `unrecognized token: "^"`.
                "SELECT s ^@ 'a' FROM t;",
                // OPERATOR(pg_catalog.+) used to emit verbatim, near "(".
                "SELECT n OPERATOR(pg_catalog.+) 1 FROM t;",
                // @? has no jsonpath engine to target and stays a refusal.
                "SELECT * FROM t WHERE payload @? '$.a';",
                // FILTER shapes, cleared by measurement during the audit.
                "SELECT count(*) FILTER (WHERE n > 0) FROM t;",
                "SELECT sum(n) FILTER (WHERE date_trunc('day', ts) IS NOT NULL) FROM t;",
                    ],
    ),
    (
        "rls-policies-phase2",
        &[
                // Policy predicates run through the forward expression
                // translator: ILIKE used to fail at apply, now() lay dormant
                // until the first view read.
                "CREATE TABLE pol1 (id INT PRIMARY KEY, s TEXT);
                 ALTER TABLE pol1 ENABLE ROW LEVEL SECURITY;
                 CREATE POLICY p ON pol1 FOR SELECT USING (s ILIKE 'a%');",
                "CREATE TABLE pol2 (id INT PRIMARY KEY, ts TIMESTAMP);
                 ALTER TABLE pol2 ENABLE ROW LEVEL SECURITY;
                 CREATE POLICY p2 ON pol2 FOR SELECT USING (ts < now());
                 INSERT INTO pol2 (id, ts) VALUES (1, '2000-01-01 00:00:00');",
                    ],
    ),
    (
        "date-arithmetic",
        &[
                // Date arithmetic without an INTERVAL used to emit `+`/`-`
                // over the text SQLite holds a date in, answering 0 and 2033.
                // These carry the new emissions: julianday differences,
                // date() over a shifted Julian day, and unixepoch with the
                // 'subsec' modifier, which is 3.42 and so inside the floor.
                "SELECT d - d FROM t;",
                "SELECT d + n FROM t;",
                "SELECT d - 7 FROM t;",
                "SELECT 7 + d FROM t;",
                "SELECT date '2026-08-07' - date '2026-08-01';",
                "SELECT extract(epoch from (ts - ts)) FROM t;",
                "SELECT date_part('epoch', ts - ts) FROM t;",
                    ],
    ),
    (
        "like-escape",
        &[
                // PostgreSQL's LIKE escapes with a backslash unless the
                // statement names another character. These carry the escape
                // the forward direction now attaches, the lowered ILIKE form
                // of it, and the empty spelling that means "no escape at
                // all", which SQLite refuses and so must be dropped.
                r"SELECT s LIKE '100\%' FROM t;",
                r"SELECT s NOT LIKE 'a\_b' FROM t;",
                r"SELECT s ILIKE '100\%' FROM t;",
                "SELECT s LIKE 'a#_b' ESCAPE '#' FROM t;",
                r"SELECT s LIKE 'a\b' ESCAPE '' FROM t;",
                r"SELECT s ILIKE 'A\B' ESCAPE '' FROM t;",
                    ],
    ),
    (
        "interval-arithmetic",
        &[
                // An interval is lowered onto PostgreSQL's own three counts,
                // months then days then time, with SQLite's month-end clamp
                // after the months. These carry every emitted shape: the
                // clamp, a merged month count, a negated field, the units
                // SQLite's modifier has no word for, a fraction that spills
                // downwards, and the leading-field spelling.
                "SELECT ts + interval '1 month' FROM t;",
                "SELECT ts - interval '1 month 1 day' FROM t;",
                "SELECT ts + interval '1 year 2 months' FROM t;",
                "SELECT ts + interval '1 month -1 day' FROM t;",
                "SELECT ts + interval '1 week' FROM t;",
                "SELECT ts + interval '1 decade' FROM t;",
                "SELECT ts + interval '1.7 months' FROM t;",
                "SELECT ts + interval '1500 ms' FROM t;",
                "SELECT ts + interval '90 minutes' FROM t;",
                "SELECT d + interval '1' month FROM t;",
                    ],
    ),
    (
        "on-conflict-do-nothing",
        &[
                // DO NOTHING keeps the upsert clause rather than becoming
                // INSERT OR IGNORE, so the conflict target has to reach
                // SQLite in a form it accepts, and a bare SELECT source has
                // to carry the WHERE SQLite needs to parse the clause at all.
                "INSERT INTO t (id) VALUES (1) ON CONFLICT (id) DO NOTHING;",
                "INSERT INTO t (id, n) SELECT id, t_id FROM u ON CONFLICT (id) DO NOTHING;",
                "INSERT INTO t (id, n) SELECT id, t_id FROM u ON CONFLICT (id) DO UPDATE SET n = 1;",
                "INSERT INTO t (id) VALUES (1) ON CONFLICT (id) DO NOTHING RETURNING id;",
                    ],
    ),
    (
        "foreign-key-match",
        &[
                // A composite MATCH FULL grows a named CHECK beside the
                // foreign key, because SQLite ignores the MATCH clause. The
                // other two rows must not grow one, and they carry their own
                // parent tables since the shared fixture has no composite key.
                // MATCH PARTIAL is deliberately absent: it is refused, and a
                // refused row reaches neither the sweep nor the floor.
                "CREATE TABLE fkm_parent (a INT, b INT, PRIMARY KEY (a, b));
                 CREATE TABLE fkm_child (x INT, y INT,
                     FOREIGN KEY (x, y) REFERENCES fkm_parent (a, b) MATCH FULL);
                 INSERT INTO fkm_child (x, y) VALUES (NULL, NULL);",
                "CREATE TABLE fkm_parent2 (a INT, b INT, PRIMARY KEY (a, b));
                 CREATE TABLE fkm_child2 (x INT, y INT,
                     FOREIGN KEY (x, y) REFERENCES fkm_parent2 (a, b) MATCH SIMPLE);",
                "CREATE TABLE fkm_parent3 (a INT PRIMARY KEY);
                 CREATE TABLE fkm_child3 (x INT REFERENCES fkm_parent3 (a) MATCH FULL);",
                    ],
    ),
    (
        "serial-columns",
        &[
                // The shapes SQLite really does auto-assign, all three of
                // which must keep emitting and inserting without naming a
                // value. A serial off the primary key is refused, so it is
                // deliberately absent: a refused row reaches neither the sweep
                // nor the floor.
                "CREATE TABLE ser1 (n SERIAL PRIMARY KEY, tag TEXT);
                 INSERT INTO ser1 (tag) VALUES ('a'), ('b');",
                "CREATE TABLE ser2 (n SERIAL, tag TEXT, PRIMARY KEY (n));
                 INSERT INTO ser2 (tag) VALUES ('a'), ('b');",
                "CREATE TABLE ser3 (n INT GENERATED BY DEFAULT AS IDENTITY, tag TEXT,
                     PRIMARY KEY (n));
                 INSERT INTO ser3 (tag) VALUES ('a'), ('b');",
                    ],
    ),
    (
        "boolean-to-text",
        &[
                // A boolean rendered as text reads the word, so the cast grows
                // a CASE and the concat operand is wrapped. The last two rows
                // are the shapes that must NOT grow one, since only a boolean
                // operand is rewritten.
                "SELECT CAST(b AS TEXT) FROM t;",
                "SELECT CAST(TRUE AS TEXT), FALSE::text FROM t;",
                "SELECT CAST(n > 1 AS TEXT) FROM t;",
                "SELECT 'x' || b, b || 'x' FROM t;",
                "SELECT CAST(n AS TEXT), n || 'x' FROM t;",
                "SELECT s || 'x' FROM t;",
                    ],
    ),
    (
        "rls-view-reads",
        &[
                // A declared view over an RLS table reads the backing table,
                // as PostgreSQL does, which is what lets a policy consult its
                // own table through one. A `security_invoker` view keeps
                // reading the policy view. The self-referential policy itself
                // is refused, so it is deliberately absent: a refused row
                // reaches neither the sweep nor the floor.
                "CREATE TABLE vr (id INTEGER PRIMARY KEY, owner_id INT);
                 ALTER TABLE vr ENABLE ROW LEVEL SECURITY;
                 CREATE VIEW vr_all AS SELECT id, owner_id FROM vr;
                 CREATE POLICY vr_p ON vr FOR SELECT USING (
                     EXISTS (SELECT 1 FROM vr_all a WHERE a.id = vr.id AND a.owner_id > 0));
                 CREATE POLICY vr_w ON vr FOR INSERT WITH CHECK (true);
                 INSERT INTO vr (id, owner_id) VALUES (1, 2);",
                "CREATE TABLE vi (id INTEGER PRIMARY KEY, owner_id INT);
                 ALTER TABLE vi ENABLE ROW LEVEL SECURITY;
                 CREATE POLICY vi_p ON vi FOR SELECT USING (owner_id > 0);
                 CREATE VIEW vi_seen WITH (security_invoker = true) AS SELECT id FROM vi;",
                    ],
    ),
    (
        "plpgsql-scanner-and-binding",
        &[
                // Each row is one of F9's four defects, all of which only
                // surfaced when the emitted trigger ran: a variable in an
                // INSERT ... SELECT source, an identifier ending in a keyword,
                // a dollar-quoted literal, and a variable defined in terms of
                // another whose name begins with a keyword.
                "CREATE TABLE ev (id INT PRIMARY KEY);
                 CREATE TABLE au (label TEXT, src TEXT);
                 CREATE FUNCTION pf1() RETURNS trigger LANGUAGE plpgsql AS $$
                 DECLARE v_label TEXT := 'processed';
                 BEGIN
                   INSERT INTO au (label, src) SELECT v_label, 'body' FROM (SELECT 1) AS d;
                   RETURN NEW;
                 END; $$;
                 CREATE TRIGGER pt1 AFTER INSERT ON ev FOR EACH ROW EXECUTE FUNCTION pf1();
                 INSERT INTO ev VALUES (1);",
                "CREATE TABLE ev2 (id INT PRIMARY KEY);
                 CREATE TABLE d2 (id INT, preelsif TEXT);
                 CREATE FUNCTION pf2() RETURNS trigger LANGUAGE plpgsql AS $$
                 BEGIN
                   INSERT INTO d2 (id, preelsif) VALUES (NEW.id, 'ok');
                   RETURN NEW;
                 END; $$;
                 CREATE TRIGGER pt2 AFTER INSERT ON ev2 FOR EACH ROW EXECUTE FUNCTION pf2();
                 INSERT INTO ev2 VALUES (1);",
                "CREATE TABLE ev3 (id INT PRIMARY KEY);
                 CREATE TABLE lg (sql_text TEXT);
                 CREATE FUNCTION pf3() RETURNS trigger LANGUAGE plpgsql AS $$
                 BEGIN
                   INSERT INTO lg (sql_text) VALUES ($q$CASE WHEN x ELSIF y THEN 1 END$q$);
                   RETURN NEW;
                 END; $$;
                 CREATE TRIGGER pt3 AFTER INSERT ON ev3 FOR EACH ROW EXECUTE FUNCTION pf3();
                 INSERT INTO ev3 VALUES (1);",
                "CREATE TABLE ev4 (id INT PRIMARY KEY, amount INT);
                 CREATE TABLE au4 (label TEXT, src TEXT);
                 CREATE FUNCTION pf4() RETURNS trigger LANGUAGE plpgsql AS $$
                 DECLARE
                   SELECT_FACTOR FLOAT := 2.0;
                   v_result FLOAT;
                 BEGIN
                   IF NEW.id = 1 THEN
                     v_result := (SELECT_FACTOR * NEW.amount);
                     INSERT INTO au4 (label, src) VALUES ('a', CAST(v_result AS TEXT));
                   ELSIF NEW.id = 2 THEN
                     INSERT INTO au4 (label, src) VALUES ('b', CAST(v_result AS TEXT));
                   END IF;
                   RETURN NEW;
                 END; $$;
                 CREATE TRIGGER pt4 AFTER INSERT ON ev4 FOR EACH ROW EXECUTE FUNCTION pf4();
                 INSERT INTO ev4 VALUES (2, 10);",
                    ],
    ),
    (
        "statistical-aggregates",
        &[
                // The three clauses the old closed forms discarded, plus a
                // grouped call. Each name reaches SQLite verbatim because the
                // sweep options declare it, so what these prove is that the
                // call arrives whole rather than rebuilt.
                "SELECT var_pop(r) OVER (PARTITION BY n) FROM t;",
                "SELECT stddev_samp(r) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) \
                 FROM t;",
                "SELECT var_pop(DISTINCT r) FROM t;",
                "SELECT n, corr(r, r) FROM t GROUP BY n HAVING stddev_pop(r) > 0;",
                "SELECT variance(r) FILTER (WHERE n > 0) FROM t;",
            ],
    ),
    (
        "uuid-version",
        &[
                // The two generators reach two names. Each row carries its own
                // UUID table, since the shared fixture has no UUID column, and
                // the column default is the shape SQLite's DDL grammar is
                // fussiest about, since it only takes a parenthesised call.
                "CREATE TABLE uv (id UUID PRIMARY KEY DEFAULT uuidv7(), label TEXT);
                 INSERT INTO uv (label) VALUES ('a');",
                "CREATE TABLE uw (id UUID PRIMARY KEY DEFAULT gen_random_uuid(), label TEXT);
                 INSERT INTO uw (label) VALUES ('a');",
                "SELECT uuidv7(), gen_random_uuid(), uuidv4(), uuid_generate_v4();",
                "CREATE TABLE ux (id INT PRIMARY KEY);
                 CREATE TABLE uy (id UUID);
                 CREATE FUNCTION uf() RETURNS trigger LANGUAGE plpgsql AS $$
                 DECLARE v_id UUID := uuidv7();
                 BEGIN
                   INSERT INTO uy (id) VALUES (v_id);
                   RETURN NEW;
                 END; $$;
                 CREATE TRIGGER ut AFTER INSERT ON ux FOR EACH ROW EXECUTE FUNCTION uf();
                 INSERT INTO ux VALUES (1);",
            ],
    ),
];
