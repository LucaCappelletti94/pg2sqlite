//! The crate's central guarantee, enforced over a corpus of PostgreSQL
//! constructs: if translation succeeds, SQLite must accept the result.
//!
//! For every construct there are exactly two acceptable outcomes:
//!
//! * translation returns `Err`, because the construct has no SQLite form, or
//! * translation succeeds and SQLite prepares every emitted statement.
//!
//! Emitting SQL that SQLite cannot parse, or naming a function it does not
//! have, is the failure this test exists to catch. Panicking is also a failure:
//! the public API returns `Result` and must never unwind.
//!
//! Constructs are executed as generated text through `rusqlite` because the
//! string the translator produced is the artifact under test.

use pg2sqlite::prelude::{
    ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions, TranslationOptions, UuidRepresentation,
};
use rusqlite::{Connection, functions::FunctionFlags};

/// Declared up front so no case fails merely for naming an unknown relation.
const FIXTURE: &str = "
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
/// lacking an opt-in.
fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_array_representation(ArrayRepresentation::Json)
        .with_math_functions_available()
        .with_rls_audit_table_name("rls_violations")
}

/// A SQLite complaint that means the translator emitted something wrong, as
/// opposed to the corpus referencing a name it never declared.
fn is_translator_fault(msg: &str) -> bool {
    msg.contains("syntax error") || msg.contains("unrecognized token") || msg.contains("no such")
}

fn sqlite_accepts(setup: &[String], stmts: &[String]) -> Result<(), String> {
    let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
    // Register sqrt and pow so the sweep can validate SQL emitted when math
    // functions are enabled in the sweep options. Both are needed because ^ and
    // ||/ translate to pow(), and stddev/corr translate to sqrt().
    conn.create_scalar_function("sqrt", 1, FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        let x: f64 = ctx.get(0)?;
        Ok(x.sqrt())
    })
    .map_err(|e| format!("sqrt UDF registration failed: {e}"))?;
    conn.create_scalar_function("pow", 2, FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        let base: f64 = ctx.get(0)?;
        let exp: f64 = ctx.get(1)?;
        Ok(base.powf(exp))
    })
    .map_err(|e| format!("pow UDF registration failed: {e}"))?;
    for s in setup {
        conn.execute_batch(&format!("{s};")).map_err(|e| format!("setup rejected: {e}"))?;
    }
    for s in stmts {
        // `prepare` checks syntax, function names, and column names without
        // running anything. DDL and trigger bodies are only checked when
        // executed, so fall back to that.
        let Err(prepare_err) = conn.prepare(s) else { continue };
        let msg = prepare_err.to_string();
        if is_translator_fault(&msg) {
            return Err(format!("{msg}\n         emitted: {s}"));
        }
        if let Err(exec_err) = conn.execute_batch(&format!("{s};")) {
            let msg = exec_err.to_string();
            if is_translator_fault(&msg) {
                return Err(format!("{msg}\n         emitted: {s}"));
            }
        }
    }
    Ok(())
}

/// Translate each case against the fixture and report the ones SQLite rejects.
fn sweep(label: &str, cases: &[&str]) -> Vec<String> {
    let opts = options();
    let setup: Vec<String> = Pg2Sqlite::default()
        .sql(FIXTURE)
        .expect("fixture parses")
        .translate_to_sql(&opts)
        .expect("fixture translates");

    let mut bugs = Vec::new();
    for case in cases {
        let full = format!("{FIXTURE}\n{case}");
        let outcome = std::panic::catch_unwind(|| {
            Pg2Sqlite::default().sql(&full).and_then(|p| p.translate_to_sql(&opts))
        });
        let all = match outcome {
            // A panic escaping the public API is a defect regardless of what
            // the construct is.
            Err(_) => {
                bugs.push(format!("[{label}] {case}\n      -> PANIC, must return Err"));
                continue;
            }
            // An explicit rejection is a valid outcome.
            Ok(Err(_)) => continue,
            Ok(Ok(all)) => all,
        };
        let emitted = &all[setup.len().min(all.len())..];
        if let Err(why) = sqlite_accepts(&setup, emitted) {
            bugs.push(format!("[{label}] {case}\n      -> {why}"));
        }
    }
    bugs
}

fn report(groups: &[(&str, &[&str])]) {
    let mut bugs = Vec::new();
    for (label, cases) in groups {
        bugs.extend(sweep(label, cases));
    }
    assert!(
        bugs.is_empty(),
        "{} construct(s) produced SQL SQLite rejects:\n\n{}\n",
        bugs.len(),
        bugs.join("\n")
    );
}

#[test]
fn dml_constructs() {
    report(&[(
        "dml",
        &[
            "INSERT INTO t (id) VALUES (1), (2), (3);",
            "INSERT INTO t DEFAULT VALUES;",
            "INSERT INTO t (id) VALUES (1) ON CONFLICT DO NOTHING;",
            "INSERT INTO t (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET n = 1;",
            "INSERT INTO t (id) VALUES (1) ON CONFLICT (id) WHERE n > 0 DO UPDATE SET n = 1;",
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
    )]);
}

#[test]
fn query_constructs() {
    report(&[(
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
    )]);
}

#[test]
fn window_constructs() {
    report(&[(
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
    )]);
}

#[test]
fn operator_constructs() {
    report(&[(
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
    )]);
}

#[test]
fn json_operator_constructs() {
    report(&[(
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
    )]);
}

#[test]
fn expression_constructs() {
    report(&[(
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
    )]);
}

#[test]
fn function_constructs() {
    report(&[(
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
    )]);
}

#[test]
fn ddl_constructs() {
    report(&[(
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
    )]);
}

#[test]
fn type_constructs() {
    report(&[(
        "types",
        &[
            "CREATE TABLE a (x SMALLINT, y INTEGER, z BIGINT);",
            "CREATE TABLE a (x DECIMAL(10, 2), y NUMERIC(10, 2));",
            "CREATE TABLE a (x CHAR(3), y VARCHAR(10), z TEXT);",
            "CREATE TABLE a (x DATE, y TIME, z TIMESTAMPTZ);",
            "CREATE TABLE a (x BYTEA, y BIT(8), z BIT VARYING(8));",
            "CREATE TABLE a (x UUID, y JSON, z JSONB);",
            "CREATE TABLE a (x SERIAL PRIMARY KEY, y BIGSERIAL);",
            "CREATE TABLE a (x INT[], z INT ARRAY[4]);",
            "CREATE TABLE a (x TSVECTOR, y TSQUERY);",
        ],
    )]);
}
