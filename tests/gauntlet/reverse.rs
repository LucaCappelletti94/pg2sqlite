//! Gauntlet C: the reverse translator's output is not merely parseable as
//! PostgreSQL, it is something PostgreSQL will take.
//!
//! Every case here is harvested from one of the 23 reverse-translation test
//! files that already exist. Those tests check that the output parses with
//! sqlparser's PostgreSQL dialect; this file goes one step further and asks a
//! real server. PREPARE validates names, types, and functions without leaving
//! state behind. Execute cases are self-contained SELECTs whose results prove
//! the function resolved correctly.
//!
//! A case PostgreSQL refuses is a finding about the reverse translator. It is
//! kept in the list as a `KnownRefusal` with the server's own words, so the
//! list documents the state rather than hiding it.

#![allow(clippy::too_many_lines)]

use diesel::{
    QueryableByName, RunQueryDsl, sql_query,
    sql_types::{Array, Text},
};
use pg2sqlite::{
    impls::sqlite_functions::shared_with_postgres,
    prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping},
    traits::TranslationOptions,
};
use postgres_harness::{apply, fresh_database};
use sql_traits::structs::ParserDB;

use crate::postgres_harness;

/// Union of all table shapes referenced by the harvested cases. Applied once to
/// a fresh database and reused for every case.
const SCHEMA_DDL: &str = "
CREATE TABLE t (
    id      INTEGER PRIMARY KEY,
    s       TEXT,
    n       INTEGER,
    r       REAL,
    payload JSONB,
    ts      TIMESTAMP,
    tz      TIMESTAMPTZ,
    a       INTEGER,
    b       INTEGER,
    c       INTEGER,
    d       INTEGER
);
CREATE TABLE t2 (id INTEGER, c INTEGER, t1_id INTEGER, a_id INTEGER, value TEXT);
CREATE TABLE t3 (id INTEGER, t2_id INTEGER, b_id INTEGER);
CREATE TABLE users (
    id     INTEGER PRIMARY KEY,
    name   TEXT,
    age    INTEGER,
    score  REAL,
    email  TEXT,
    active BOOLEAN
);
CREATE TABLE posts (
    id      INTEGER PRIMARY KEY,
    user_id INTEGER,
    title   TEXT
);
CREATE TABLE tags (
    id       INTEGER PRIMARY KEY,
    name     TEXT,
    category TEXT,
    post_id  INTEGER,
    tag      TEXT
);
CREATE TABLE events (
    id         INTEGER PRIMARY KEY,
    created_at TIMESTAMP
);
CREATE TABLE docs (
    id      INTEGER PRIMARY KEY,
    content TEXT
);
CREATE TABLE items (
    id       INTEGER PRIMARY KEY,
    category TEXT,
    price    INTEGER
);
CREATE TABLE orders (
    id      INTEGER PRIMARY KEY,
    user_id INTEGER,
    total   INTEGER
);
CREATE TABLE u (
    id   INTEGER PRIMARY KEY,
    name TEXT,
    note TEXT
);
CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT);
CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT);
CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER);
CREATE TABLE c (id INTEGER PRIMARY KEY, b_id INTEGER);
CREATE TABLE user_roles (
    user_id    INTEGER,
    role_id    INTEGER,
    granted_at TIMESTAMP,
    PRIMARY KEY (user_id, role_id)
);
CREATE TABLE readings (
    id     INTEGER PRIMARY KEY,
    sensor TEXT    NOT NULL,
    ts_val INTEGER NOT NULL,
    value  INTEGER NOT NULL
);
CREATE TABLE callers (
    id         INTEGER PRIMARY KEY,
    owner      TEXT,
    owner_uuid UUID
);
";

fn build_schema() -> ParserDB {
    Pg2Sqlite::default()
        .sql(SCHEMA_DDL)
        .expect("schema DDL parses")
        .build_schema()
        .expect("schema builds")
}

/// All cases PostgreSQL is expected to accept, validated by PREPARE/DEALLOCATE.
/// Sources are noted inline.
///
/// Format: (sqlite_input, source_file_hint)
const ACCEPT_CASES: &[(&str, &str)] = &[
    // --- the date and time parts, which cross as casts because PostgreSQL
    // refuses `time(x)`, `time` being a type name there
    // (test_reverse_unknown_functions.rs)
    ("SELECT date(ts) FROM t", "date_and_time"),
    ("SELECT time(ts) FROM t", "date_and_time"),
    ("SELECT date() FROM t", "date_and_time"),
    ("SELECT time() FROM t", "date_and_time"),
    // --- json functions (test_reverse_json_functions.rs,
    // test_reverse_output_is_valid_postgres.rs)
    // json_set and json_insert now wrap the value in to_jsonb(); json_type
    // chooses jsonb_typeof for JSONB columns; hex casts the argument to bytea.
    ("SELECT json(s) FROM t", "json_functions"),
    ("SELECT json_set(payload, '$.a', 1) FROM t", "json_functions"),
    ("SELECT json_insert(payload, '$.a', 1) FROM t", "json_functions"),
    ("SELECT json_set(payload, '$.a.b', 1) FROM t", "json_functions"),
    ("SELECT json_type(payload) FROM t", "json_functions"),
    ("SELECT json_remove(payload, '$.a') FROM t", "json_functions"),
    ("SELECT json_extract(payload, '$.a') FROM t", "json_functions"),
    ("SELECT json_quote(s) FROM t", "json_functions"),
    ("SELECT json_valid(s) FROM t", "json_functions"),
    ("SELECT json_patch(payload, payload) FROM t", "json_functions"),
    ("SELECT json_array_length(payload) FROM t", "json_functions"),
    ("SELECT json_group_array(s) FROM t", "json_functions"),
    ("SELECT json_array(s) FROM t", "json_functions"),
    ("SELECT json_extract(payload, '$.a.b') FROM t", "json_functions"),
    ("SELECT json_remove(payload, '$.a.b') FROM t", "json_functions"),
    // --- scalar functions (test_reverse_scalar_functions.rs,
    // test_reverse_output_is_valid_postgres.rs)
    ("SELECT ifnull(n, 0) FROM t", "scalar_functions"),
    ("SELECT total(n) FROM t", "scalar_functions"),
    ("SELECT unhex(s) FROM t", "scalar_functions"),
    ("SELECT instr(s, 'a') FROM t", "scalar_functions"),
    ("SELECT unicode(s) FROM t", "scalar_functions"),
    ("SELECT min(n, 1) FROM t", "scalar_functions"),
    ("SELECT max(n, 1) FROM t", "scalar_functions"),
    ("SELECT nullif(n, 0) FROM t", "scalar_functions"),
    ("SELECT group_concat(s, ',') FROM t", "scalar_functions"),
    ("SELECT group_concat(s) FROM t", "scalar_functions"),
    ("SELECT group_concat(DISTINCT s) FROM t", "scalar_functions"),
    // hex(x) now casts the argument to bytea so encode() accepts it.
    ("SELECT hex(s) FROM t", "scalar_functions"),
    // --- strftime (test_reverse_strftime.rs, test_reverse_output_is_valid_postgres.rs)
    ("SELECT strftime('%Y-01-01 00:00:00', ts) FROM t", "strftime"),
    ("SELECT strftime('%Y', ts) FROM t", "strftime"),
    ("SELECT strftime('%Y-%m-%d', ts) FROM t", "strftime"),
    ("SELECT strftime('%H:%M:%S', ts) FROM t", "strftime"),
    ("SELECT strftime('%Y-%m-%dT%H', ts) FROM t", "strftime"),
    ("SELECT strftime('%I:%M', ts) FROM t", "strftime"),
    // --- epoch (test_reverse_epoch_now.rs)
    ("SELECT unixepoch(ts) FROM t", "epoch_now"),
    ("SELECT unixepoch(ts, 'subsec') FROM t", "epoch_now"),
    ("SELECT datetime(tz) FROM t", "epoch_now"),
    ("SELECT datetime(ts) FROM t", "epoch_now"),
    // --- string_agg (test_reverse_string_agg.rs)
    ("SELECT group_concat(name) FROM tags", "string_agg"),
    ("SELECT group_concat(name, '|') FROM tags", "string_agg"),
    ("SELECT group_concat(DISTINCT name) FROM tags", "string_agg"),
    ("SELECT group_concat(name ORDER BY name DESC) FROM tags", "string_agg"),
    ("SELECT group_concat(name) OVER (ORDER BY id) FROM tags", "string_agg"),
    // --- GLOB to LIKE (test_reverse_output_is_valid_postgres.rs,
    // test_reverse_scalar_functions.rs)
    ("SELECT s FROM t WHERE s GLOB 'a*'", "glob"),
    ("SELECT s FROM t WHERE s GLOB 'a?b'", "glob"),
    // --- REGEXP to POSIX (test_reverse_output_is_valid_postgres.rs, test_reverse_expr.rs)
    ("SELECT s FROM t WHERE s REGEXP '^[A-Z]'", "regexp"),
    ("SELECT s FROM t WHERE s NOT REGEXP '^[A-Z]'", "regexp"),
    // --- INSERT OR REPLACE / OR IGNORE (test_reverse_output_is_valid_postgres.rs)
    ("INSERT OR IGNORE INTO t (id, n) VALUES (1, 42)", "insert_or_ignore"),
    ("INSERT OR REPLACE INTO t (id, s, n) VALUES (1, 'x', 42)", "insert_or_replace"),
    ("INSERT OR REPLACE INTO t (id) VALUES (1)", "insert_or_replace"),
    ("REPLACE INTO t (id, s) VALUES (1, 'x')", "insert_or_replace"),
    // --- placeholders (test_reverse_placeholders.rs)
    ("SELECT * FROM t WHERE a > ? AND b = ?", "placeholders"),
    ("SELECT * FROM t WHERE a > ?2 AND b = ?1", "placeholders"),
    ("SELECT * FROM t LIMIT ? OFFSET ?", "placeholders"),
    ("SELECT * FROM t WHERE a IN (?, ?)", "placeholders"),
    ("SELECT * FROM t WHERE a BETWEEN ? AND ?", "placeholders"),
    ("UPDATE t SET a = ?, b = ? WHERE c = ?", "placeholders"),
    ("DELETE FROM t WHERE a = ?", "placeholders"),
    ("INSERT INTO t (a, b) VALUES (?, ?)", "placeholders"),
    ("WITH x AS (SELECT a FROM t WHERE b = ?) SELECT * FROM x WHERE a = ?", "placeholders"),
    // --- query features (test_reverse_query.rs)
    ("SELECT * FROM users ORDER BY name ASC", "query"),
    ("SELECT * FROM users ORDER BY age DESC", "query"),
    ("SELECT id FROM users UNION SELECT id FROM users", "query"),
    ("SELECT id FROM users UNION ALL SELECT id FROM users", "query"),
    ("SELECT id FROM users INTERSECT SELECT id FROM users", "query"),
    ("SELECT id FROM users EXCEPT SELECT id FROM users", "query"),
    ("SELECT age, COUNT(*) FROM users GROUP BY age HAVING COUNT(*) > 1", "query"),
    ("SELECT age, COUNT(*) FROM users GROUP BY age", "query"),
    ("SELECT * FROM (SELECT id, name FROM users) AS sub", "query"),
    ("SELECT name AS user_name FROM users", "query"),
    ("SELECT u.name, p.title FROM users u INNER JOIN posts p ON u.id = p.user_id", "query"),
    ("SELECT u.name, p.title FROM users u LEFT JOIN posts p ON u.id = p.user_id", "query"),
    ("SELECT * FROM users CROSS JOIN posts", "query"),
    ("SELECT u.name, p.title FROM users u RIGHT OUTER JOIN posts p ON u.id = p.user_id", "query"),
    ("SELECT u.name, p.title FROM users u FULL OUTER JOIN posts p ON u.id = p.user_id", "query"),
    ("SELECT * FROM users NATURAL JOIN posts", "query"),
    ("SELECT * FROM t JOIN t2 USING (c)", "query"),
    ("SELECT * FROM users LIMIT 10", "query"),
    ("SELECT * FROM users LIMIT 10 OFFSET 5", "query"),
    ("SELECT DISTINCT name FROM users", "query"),
    ("INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30), (2, 'Bob', 25)", "query"),
    ("SELECT * FROM users LEFT OUTER JOIN posts ON users.id = posts.user_id", "query"),
    (
        "SELECT name, (SELECT COUNT(*) FROM posts WHERE posts.user_id = users.id) AS post_count FROM users",
        "query",
    ),
    ("SELECT name AS user_name, age AS user_age FROM users", "query"),
    (
        "SELECT users.name, posts.title, COUNT(*) FROM users JOIN posts ON users.id = posts.user_id GROUP BY users.name, posts.title",
        "query",
    ),
    ("SELECT name, SUM(age) FROM users GROUP BY name HAVING SUM(age) > 100", "query"),
    (
        "SELECT * FROM (SELECT id, name FROM users WHERE age > 18) AS adults WHERE adults.name LIKE 'A%'",
        "query",
    ),
    (
        "SELECT id, name FROM users WHERE age > 30 UNION ALL SELECT id, name FROM users WHERE age < 10 ORDER BY name",
        "query",
    ),
    ("SELECT * FROM users", "query"),
    (
        "SELECT SUM(u.id) OVER w2 FROM users u WINDOW w1 AS (PARTITION BY u.id ORDER BY u.id), w2 AS (w1)",
        "query",
    ),
    // --- DML (test_reverse_dml.rs)
    ("DELETE FROM users WHERE id = 1", "dml"),
    ("DELETE FROM users WHERE id IN (SELECT user_id FROM posts WHERE title = 'test')", "dml"),
    ("DELETE FROM users", "dml"),
    ("UPDATE users SET name = 'test' WHERE id = 1", "dml"),
    ("UPDATE users SET age = age + 1 WHERE id = 1", "dml"),
    ("UPDATE users SET name = 'Bob', age = 30 WHERE id = 1", "dml"),
    ("INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30)", "dml"),
    ("INSERT OR IGNORE INTO users (id, name, age) VALUES (1, 'Alice', 30)", "dml"),
    ("INSERT OR REPLACE INTO users (id, name, age) VALUES (1, 'Alice', 30)", "dml"),
    ("INSERT INTO users (id, name, age) SELECT id, title, 0 FROM posts", "dml"),
    (
        "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30) ON CONFLICT (id) DO UPDATE SET name = excluded.name WHERE users.age > 18",
        "dml",
    ),
    ("DELETE FROM users WHERE id = 1 RETURNING *", "dml"),
    ("DELETE FROM users WHERE id = 1 RETURNING id, name", "dml"),
    ("DELETE FROM users WHERE id = 1 RETURNING id AS deleted_id", "dml"),
    ("UPDATE users SET name = 'test' WHERE id = 1 RETURNING *", "dml"),
    ("UPDATE users SET name = 'test' WHERE id = 1 RETURNING name AS updated_name", "dml"),
    ("INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30) RETURNING *", "dml"),
    ("INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30) RETURNING id AS new_id", "dml"),
    ("DELETE FROM users WHERE EXISTS (SELECT 1 FROM posts WHERE posts.user_id = users.id)", "dml"),
    ("UPDATE users SET name = posts.title FROM posts WHERE users.id = posts.user_id", "dml"),
    ("INSERT INTO users (id, name, age) SELECT p.id, p.title, 0 FROM posts p", "dml"),
    ("INSERT INTO users (id, name, age) SELECT id, title || ' author', 0 FROM posts", "dml"),
    ("DELETE FROM users WHERE age < 18 RETURNING id, name || ' deleted' AS msg", "dml"),
    ("UPDATE users SET age = age + 1 WHERE id = 1 RETURNING id, name, age AS new_age", "dml"),
    (
        "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30) RETURNING id, name AS inserted_name",
        "dml",
    ),
    (
        "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30) ON CONFLICT (id) DO NOTHING",
        "dml",
    ),
    ("UPDATE users SET name = 'updated', age = 99 WHERE id > 5 AND name LIKE '%test%'", "dml"),
    // --- expressions (test_reverse_expr.rs)
    ("SELECT NOT (age > 5) FROM users", "expr"),
    ("SELECT -age FROM users", "expr"),
    ("SELECT (age + 1) FROM users", "expr"),
    ("SELECT age + score FROM users", "expr"),
    ("SELECT * FROM users WHERE age > 5 AND name = 'test'", "expr"),
    ("SELECT CAST(age AS TEXT) FROM users", "expr"),
    ("SELECT * FROM users WHERE name IS NULL", "expr"),
    ("SELECT * FROM users WHERE name IS NOT NULL", "expr"),
    ("SELECT * FROM users WHERE (age > 0) IS TRUE", "expr"),
    ("SELECT * FROM users WHERE (age > 0) IS NOT TRUE", "expr"),
    ("SELECT * FROM users WHERE (age > 0) IS FALSE", "expr"),
    ("SELECT * FROM users WHERE (age > 0) IS NOT FALSE", "expr"),
    ("SELECT * FROM users WHERE EXISTS (SELECT 1 FROM users WHERE age > 5)", "expr"),
    ("SELECT * FROM users WHERE NOT EXISTS (SELECT 1 FROM users WHERE age > 5)", "expr"),
    ("SELECT * FROM users WHERE name LIKE '%test%'", "expr"),
    ("SELECT * FROM users WHERE name NOT LIKE '%test%'", "expr"),
    ("SELECT * FROM users WHERE age IN (1, 2, 3)", "expr"),
    ("SELECT * FROM users WHERE age NOT IN (1, 2, 3)", "expr"),
    ("SELECT * FROM users WHERE id IN (SELECT id FROM users WHERE age > 5)", "expr"),
    ("SELECT * FROM users WHERE age BETWEEN 10 AND 20", "expr"),
    ("SELECT CASE WHEN age > 18 THEN 'adult' ELSE 'minor' END FROM users", "expr"),
    ("SELECT CASE age WHEN 18 THEN 'eighteen' WHEN 21 THEN 'twentyone' END FROM users", "expr"),
    ("SELECT (SELECT MAX(age) FROM users) AS max_age", "expr"),
    ("SELECT TRIM(name) FROM users", "expr"),
    ("SELECT POSITION('a' IN name) FROM users", "expr"),
    ("SELECT SUBSTRING(name FROM 1 FOR 3) FROM users", "expr"),
    (
        "SELECT * FROM users WHERE (age > 18 AND name IS NOT NULL) OR score BETWEEN 0.0 AND 100.0",
        "expr",
    ),
    ("SELECT * FROM users WHERE name ILIKE '%test%'", "expr"),
    ("SELECT * FROM users WHERE name NOT ILIKE '%test%'", "expr"),
    ("SELECT EXTRACT(YEAR FROM created_at) FROM events", "expr"),
    ("SELECT * FROM users WHERE (id, age) IN ((1, 30), (2, 25))", "expr"),
    ("SELECT TRIM(LEADING ' ' FROM name) FROM users", "expr"),
    ("SELECT TRIM(BOTH ' ' FROM name) FROM users", "expr"),
    ("SELECT CEIL(score) FROM users", "expr"),
    ("SELECT FLOOR(score) FROM users", "expr"),
    ("SELECT * FROM users WHERE name REGEXP '^[A-Z]'", "expr"),
    ("SELECT users.name FROM users", "expr"),
    ("SELECT * FROM events WHERE created_at > DATE '2024-01-01'", "expr"),
    ("SELECT * FROM events WHERE created_at > TIMESTAMP '2024-01-01 00:00:00'", "expr"),
    ("SELECT SUBSTRING(name, 1, 3) FROM users", "expr"),
    (
        "SELECT CASE WHEN age < 13 THEN 'child' WHEN age < 18 THEN 'teen' WHEN age < 65 THEN 'adult' ELSE 'senior' END FROM users",
        "expr",
    ),
    ("SELECT * FROM users WHERE age NOT BETWEEN 10 AND 20", "expr"),
    ("SELECT * FROM users WHERE id NOT IN (SELECT id FROM users WHERE age < 18)", "expr"),
    ("SELECT * FROM users WHERE ((age > 5) AND (name IS NOT NULL)) OR (score < 10.0)", "expr"),
    // --- ARRAY literal (test_reverse_expr.rs)
    // The SQLite dialect misparsed ARRAY[...] as an identifier with a bracket
    // alias; the reverse translator now reconstructs the original array.
    // --- LIMIT comma form (test_reverse_limit_comma.rs)
    ("SELECT id FROM t ORDER BY id LIMIT 5, 10", "limit_comma"),
    ("SELECT id FROM t ORDER BY id LIMIT 10 OFFSET 5", "limit_comma"),
    // --- ident quoting (test_reverse_ident_quoting.rs)
    (r"SELECT `t`.`c` FROM `t` WHERE `t`.`c` > 1 ORDER BY `t`.`c`", "ident_quoting"),
    (r"SELECT [t].[c] FROM [t] WHERE [t].[c] > 1 ORDER BY [t].[c]", "ident_quoting"),
    (r"INSERT INTO `t` (`c`) VALUES (1) RETURNING `c`", "ident_quoting"),
    (r"UPDATE [t] SET [c] = 2 WHERE [c] = 1", "ident_quoting"),
    (r"DELETE FROM `t` WHERE `c` = 1", "ident_quoting"),
    (r"WITH `cte` AS (SELECT `c` FROM `t`) SELECT `c` FROM `cte`", "ident_quoting"),
    (
        r"SELECT `x`.`c` FROM `t` AS `x` JOIN `t` AS `y` ON `x`.`c` = `y`.`c` WHERE `x`.`c` IN (SELECT `c` FROM `t`)",
        "ident_quoting",
    ),
    // --- OR REPLACE (test_reverse_or_replace.rs, no triggers in schema so no refusal)
    ("INSERT OR REPLACE INTO u VALUES (1, 'x', 'y')", "or_replace"),
    ("INSERT OR IGNORE INTO u VALUES (1, 'x', 'y')", "or_replace"),
    ("INSERT OR ABORT INTO u VALUES (1, 'x', 'y')", "or_replace"),
    // --- LIKE contract (test_reverse_like_contract.rs)
    ("SELECT * FROM t WHERE s LIKE '%test%'", "like_contract"),
    ("SELECT * FROM t WHERE s NOT LIKE '%test%'", "like_contract"),
    // --- roundtrip (test_reverse_roundtrip.rs)
    ("SELECT strftime('%Y-01-01 00:00:00', created_at) FROM events", "roundtrip"),
    ("SELECT strftime('%Y', created_at) FROM events", "roundtrip"),
    ("SELECT strftime('%m', created_at) FROM events", "roundtrip"),
    ("SELECT strftime('%d', created_at) FROM events", "roundtrip"),
    ("SELECT strftime('%H', created_at) FROM events", "roundtrip"),
    ("SELECT strftime('%M', created_at) FROM events", "roundtrip"),
    ("SELECT strftime('%S', created_at) FROM events", "roundtrip"),
    // --- translation (test_reverse_translation.rs)
    ("SELECT id, name, email FROM users WHERE name = 'Alice'", "translation"),
    ("INSERT INTO users (id, name, email) VALUES (1, 'Alice', 'alice@example.com')", "translation"),
    ("UPDATE users SET name = 'Bob', email = 'bob@example.com' WHERE id = 1", "translation"),
    ("DELETE FROM users WHERE id = 1", "translation"),
    ("SELECT * FROM events WHERE created_at > datetime('now')", "translation"),
    ("SELECT INSTR(content, 'search') FROM docs", "translation"),
    ("SELECT category, group_concat(name) FROM tags GROUP BY category", "translation"),
    ("SELECT char(65) FROM users", "translation"),
    ("SELECT u.name, p.title FROM users u JOIN posts p ON u.id = p.user_id", "translation"),
    (
        "SELECT category, COUNT(*) AS cnt FROM items GROUP BY category HAVING COUNT(*) > 1",
        "translation",
    ),
    // --- field_clones / statement_edges (test_reverse_field_clones.rs,
    // test_reverse_statement_edges.rs)
    ("SELECT c FROM t GROUP BY c HAVING c > 0", "statement_edges"),
    ("SELECT ROW_NUMBER() OVER (PARTITION BY c ORDER BY c) FROM t", "statement_edges"),
    ("SELECT COUNT(*) FILTER (WHERE c > 0) FROM t", "statement_edges"),
    ("SELECT c FROM t JOIN t2 USING (c)", "statement_edges"),
];

/// Self-contained SELECTs that are executed directly. The execution proves the
/// function exists and produces a result. The expected return value is noted in
/// the comment; the assertion is that the statement runs without error.
const EXECUTE_CASES: &[(&str, &str)] = &[
    // char(65) -> chr(65) returns 'A'
    ("SELECT char(65)", "chr(65) returns 'A'"),
    // ifnull(1, 0) -> COALESCE(1, 0) returns 1
    ("SELECT ifnull(1, 0)", "COALESCE(1, 0) returns 1"),
    // iif(1 > 0, 'yes', 'no') -> CASE WHEN returns 'yes'
    ("SELECT iif(1 > 0, 'yes', 'no')", "CASE WHEN returns 'yes'"),
    // unixepoch() -> floor(EXTRACT(EPOCH FROM NOW()))::BIGINT returns current epoch seconds
    ("SELECT unixepoch()", "returns current epoch as whole seconds"),
    // nullif(1, 0) -> nullif(1, 0) returns 1
    ("SELECT nullif(1, 0)", "returns 1"),
    // min(3, 1) -> LEAST(3, 1) returns 1
    ("SELECT min(3, 1)", "LEAST(3, 1) returns 1"),
    // max(3, 1) -> GREATEST(3, 1) returns 3
    ("SELECT max(3, 1)", "GREATEST(3, 1) returns 3"),
];

/// Cases where PostgreSQL refuses what the reverse translator emits. Each entry
/// is (sqlite_input, fragment that must appear in the refusal message). These
/// are findings about the reverse translator, not errors in the gauntlet.
const KNOWN_REFUSALS: &[(&str, &str)] = &[
    // --- a column whose name PostgreSQL reserves
    // Brackets are SQLite's identifier quoting, so this input is not an array
    // literal: it reads the column `ARRAY` under the alias `1, 2, 3`, which is
    // what SQLite answers. The reverse translation says the same thing, and
    // PostgreSQL refuses it because `ARRAY` is a reserved word that has to be
    // quoted to name a column. Quoting it is not as simple as quoting every
    // reserved word an identifier node carries, because sqlparser also carries
    // keywords such as `DEFAULT` in identifier nodes, and quoting one of those
    // turns syntax into a column reference. The fix needs to know which
    // identifiers are names, which the quoting pass currently cannot see.
    ("SELECT ARRAY[1, 2, 3] FROM users", "syntax error"),
    // --- timestamp > INTERVAL: type mismatch in PostgreSQL
    // PostgreSQL cannot compare timestamp (no time zone) with interval using >.
    // SQLite has no interval type, so this construct has no direct equivalent.
    // Whether this case belongs in the corpus at all is an open question.
    ("SELECT * FROM events WHERE created_at > INTERVAL '1' DAY", "does not exist"),
];

#[test]
fn reverse_output_runs_in_postgres() {
    let schema = build_schema();
    let options = Pg2SqliteOptions::default();
    let mut conn = fresh_database();
    apply(&mut conn, SCHEMA_DDL).expect("schema applied to fresh database");

    let mut failures: Vec<String> = Vec::new();
    let mut n: usize = 0;
    let mut refusal_count: usize = 0;

    // --- PREPARE cases ---------------------------------------------------
    for &(sqlite_input, source) in ACCEPT_CASES {
        let pg_sql = match Pg2Sqlite::default().reverse_sql(sqlite_input, &schema, &options) {
            Err(e) => {
                // The translator refused something that all existing tests accept.
                failures.push(format!("[{source}] translator refused {sqlite_input:?}: {e}"));
                n += 1;
                continue;
            }
            Ok(stmts) => stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "),
        };

        let name = format!("gauntlet_{n}");
        match apply(&mut conn, &format!("PREPARE {name} AS {pg_sql}")) {
            Ok(()) => {
                apply(&mut conn, &format!("DEALLOCATE {name}"))
                    .expect("DEALLOCATE should not fail");
            }
            Err(e) => {
                failures.push(format!(
                    "[{source}] PostgreSQL refused {sqlite_input:?}\n  translated: {pg_sql}\n  error: {e}"
                ));
            }
        }
        n += 1;
    }

    // --- Execute cases (self-contained SELECTs) ---------------------------
    for &(sqlite_input, description) in EXECUTE_CASES {
        let pg_sql = match Pg2Sqlite::default().reverse_sql(sqlite_input, &schema, &options) {
            Err(e) => {
                failures.push(format!("[execute] translator refused {sqlite_input:?}: {e}"));
                n += 1;
                continue;
            }
            Ok(stmts) => stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "),
        };
        match apply(&mut conn, &pg_sql) {
            Ok(()) => {}
            Err(e) => {
                failures.push(format!(
                    "[execute/{description}] PostgreSQL refused {sqlite_input:?}\n  translated: {pg_sql}\n  error: {e}"
                ));
            }
        }
        n += 1;
    }

    // --- Known refusals ---------------------------------------------------
    for &(sqlite_input, fragment) in KNOWN_REFUSALS {
        refusal_count += 1;
        let pg_sql = match Pg2Sqlite::default().reverse_sql(sqlite_input, &schema, &options) {
            Err(e) => {
                // The translator refusing is separate from PostgreSQL refusing.
                failures.push(format!(
                    "[known_refusal] translator refused {sqlite_input:?} (expected PostgreSQL to refuse): {e}"
                ));
                n += 1;
                continue;
            }
            Ok(stmts) => stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "),
        };

        let name = format!("gauntlet_{n}");
        match apply(&mut conn, &format!("PREPARE {name} AS {pg_sql}")) {
            Err(e) if e.to_lowercase().contains(&fragment.to_lowercase()) => {
                // Expected refusal.
            }
            Err(e) => {
                failures.push(format!(
                    "[known_refusal] PostgreSQL refused {sqlite_input:?} with unexpected message\n  expected fragment: {fragment:?}\n  actual error: {e}\n  translated: {pg_sql}"
                ));
            }
            Ok(()) => {
                apply(&mut conn, &format!("DEALLOCATE {name}")).ok();
                failures.push(format!(
                    "[known_refusal] PostgreSQL accepted {sqlite_input:?} which was expected to be refused\n  translated: {pg_sql}\n  to fix: move this to ACCEPT_CASES"
                ));
            }
        }
        n += 1;
    }

    let total = n;
    let accepted = total - refusal_count - failures.len();
    eprintln!(
        "reverse gauntlet: {total} cases, {accepted} accepted, {refusal_count} known refusals, {} failures",
        failures.len()
    );

    assert!(failures.is_empty(), "{} case(s) failed:\n\n{}", failures.len(), failures.join("\n\n"));
}

/// A name the server does not have, as the catalogue answers it.
#[derive(QueryableByName, Debug)]
struct AbsentName {
    /// The name asked about.
    #[diesel(sql_type = Text)]
    name: String,
}

/// The reverse direction emits every name in this crate's shared inventory
/// unchanged, so the server is asked whether it has them.
///
/// This is the half of the reverse catch-all that does not need judgement:
/// existence is a fact the catalogue answers. Whether the two engines agree on
/// what a name means is decided in `sqlite_functions.rs`, name by name, and is
/// not what this checks.
#[test]
fn every_shared_name_exists_in_postgres() {
    let mut connection = fresh_database();

    // The five the catalogue cannot answer for, because PostgreSQL parses them
    // as expressions rather than as functions. Evaluating them is the check.
    const EXPRESSIONS: [&str; 5] =
        ["coalesce", "nullif", "current_date", "current_time", "current_timestamp"];
    apply(
        &mut connection,
        "SELECT coalesce(NULL::int, 1), nullif(1, 1), current_date, current_time, \
         current_timestamp",
    )
    .expect("the expression-shaped names evaluate");

    let asked: Vec<String> = shared_with_postgres()
        .iter()
        .filter(|name| !EXPRESSIONS.contains(name))
        .map(|name| (*name).to_string())
        .collect();

    let absent: Vec<AbsentName> = sql_query(
        "SELECT nm AS name FROM unnest($1::text[]) AS nm \
         WHERE NOT EXISTS ( \
             SELECT 1 FROM pg_proc p \
             WHERE p.proname = nm AND p.pronamespace = 'pg_catalog'::regnamespace \
         )",
    )
    .bind::<Array<Text>, _>(asked)
    .load(&mut connection)
    .expect("the catalogue answers");

    assert!(
        absent.is_empty(),
        "the reverse direction would emit {} name(s) PostgreSQL does not have: {:?}",
        absent.len(),
        absent.iter().map(|row| row.name.as_str()).collect::<Vec<_>>()
    );
}

/// What a session variable mapping reverses into is SQL the server takes.
///
/// Each case carries its own options, since the pairing is what the case is
/// about, so they cannot ride along in `reverse_output_runs_in_postgres`.
#[test]
fn a_reversed_session_variable_runs_in_postgres() {
    let schema = build_schema();
    let mut connection = fresh_database();
    apply(&mut connection, SCHEMA_DDL).expect("schema applied to fresh database");

    let untyped = Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting("app.user_id", "app_user_id"),
    );
    let typed = Pg2SqliteOptions::default().with_session_variable(
        SessionVariableMapping::current_setting("app.user_id", "app_user_id").with_pg_type("uuid"),
    );
    let role = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_user("sqlite_user"));

    let cases: [(&str, &Pg2SqliteOptions, &str); 4] = [
        // The setting answers text, so a text column compares without a cast.
        ("SELECT id FROM callers WHERE owner = app_user_id()", &untyped, "text column"),
        // A uuid column does not: `uuid = text` is an error, which is why the
        // mapping records the type and the cast is written back.
        ("SELECT id FROM callers WHERE owner_uuid = app_user_id()", &typed, "uuid column"),
        // The role keyword, which PostgreSQL refuses with parentheses.
        ("SELECT id FROM callers WHERE owner = sqlite_user()", &role, "current_user"),
        // Inside a subquery, which is the shape a membership filter takes.
        (
            "SELECT id FROM callers WHERE id IN (SELECT id FROM callers WHERE owner = \
             app_user_id())",
            &untyped,
            "subquery",
        ),
    ];

    let mut failures: Vec<String> = Vec::new();
    for (index, (sqlite_input, options, description)) in cases.iter().enumerate() {
        let postgres = match Pg2Sqlite::default().reverse_sql(sqlite_input, &schema, options) {
            Ok(statements) => {
                statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
            }
            Err(error) => {
                failures.push(format!("[{description}] the translator refused: {error}"));
                continue;
            }
        };

        let name = format!("session_{index}");
        match apply(&mut connection, &format!("PREPARE {name} AS {postgres}")) {
            Ok(()) => {
                apply(&mut connection, &format!("DEALLOCATE {name}"))
                    .expect("DEALLOCATE should not fail");
            }
            Err(error) => {
                failures.push(format!(
                    "[{description}] PostgreSQL refused {sqlite_input:?}\n  translated: \
                     {postgres}\n  error: {error}"
                ));
            }
        }
    }

    assert!(failures.is_empty(), "{} case(s) failed:\n\n{}", failures.len(), failures.join("\n\n"));
}
