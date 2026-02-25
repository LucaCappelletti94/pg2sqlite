//! Comparison benchmark: pg2sqlite vs polyglot-sql vs sqlglot.
//!
//! Translates a representative set of PostgreSQL statements to SQLite using
//! three tools, executes each output against a real in-memory SQLite database,
//! and reverse-translates each successful SQLite result back to PostgreSQL.
//! The full report is printed to stdout as a Markdown document.
//!
//! # How to run
//!
//! ```text
//! cargo run --example compare_polyglot
//! ```
//!
//! Requires `python3` with `sqlglot` installed (`pip install sqlglot`) for the
//! sqlglot columns.  The pg2sqlite and polyglot columns work without Python.
//!
//! # Column legend
//!
//! | Column              | Meaning                                                          |
//! |---------------------|------------------------------------------------------------------|
//! | `pg2s→SQLite`       | pg2sqlite forward translation output                             |
//! | `pg2s runs?`        | ✓ accepted by SQLite, ✗ runtime error, — translation failed      |
//! | `pg2s RT↩PG`        | pg2sqlite's own reverse-translator applied to the SQLite output  |
//! | `poly→SQLite`       | polyglot-sql forward translation output                          |
//! | `poly runs?`        | same runtime check for polyglot output                          |
//! | `poly↩PG (pg2s out)`| polyglot SQLite→PG applied to pg2sqlite's SQLite output          |
//! | `sqlg→SQLite`       | sqlglot forward translation output                               |
//! | `sqlg runs?`        | same runtime check for sqlglot output                            |
//! | `sqlg↩PG (pg2s out)`| sqlglot SQLite→PG applied to pg2sqlite's SQLite output           |

use pg2sqlite::{options::Pg2SqliteOptions, pg2sqlite::Pg2Sqlite};
use polyglot_sql::{DialectType, transpile};
use rusqlite::Connection;

// ── Sentinel used when there is nothing to translate or test ─────────────────

const SKIPPED: &str = "—";

// ── Data types ───────────────────────────────────────────────────────────────

struct Case {
    category: &'static str,
    name: &'static str,
    sql: &'static str,
}

impl Case {
    fn new(category: &'static str, name: &'static str, sql: &'static str) -> Self {
        Self { category, name, sql }
    }
}

/// All nine translation/execution results for a single test case.
struct TranslationRow {
    pg2s: String,
    poly: String,
    sqlg: String,
    pg2s_ok: String,
    poly_ok: String,
    sqlg_ok: String,
    pg2s_rt: String,
    poly_rev: String,
    sqlg_rev: String,
}

// ── Schema fixture ───────────────────────────────────────────────────────────

/// Schema pre-populated before every non-DDL test so SELECT/UPDATE/DELETE have
/// tables and columns to reference.
const CATCH_ALL_SCHEMA: &str = "
    CREATE TABLE t (
        id, a, b, c, str, x, ts, name, val, k, v,
        active, created_at, updated_at, type, owner,
        department, salary, qty, data
    );
    CREATE TABLE emp  (id, department, salary);
    CREATE TABLE events (id, created_at);
    CREATE TABLE log  (id, val);
    CREATE TABLE mv   (id, qty);
";

// ── Forward translators ──────────────────────────────────────────────────────

/// Translate PostgreSQL SQL to SQLite using pg2sqlite.
fn translate_pg2sqlite(sql: &str) -> String {
    let translator = match Pg2Sqlite::default().sql(sql) {
        Err(e) => return format!("ERROR: {e}"),
        Ok(t) => t,
    };
    match translator.translate(&Pg2SqliteOptions::default()) {
        Ok(stmts) => stmts.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("; "),
        Err(e) => format!("ERROR: {e}"),
    }
}

/// Translate PostgreSQL SQL to SQLite using polyglot-sql.
fn translate_polyglot(sql: &str) -> String {
    match transpile(sql, DialectType::PostgreSQL, DialectType::SQLite) {
        Ok(stmts) => stmts.join("; "),
        Err(e) => format!("ERROR: {e}"),
    }
}

/// Translate PostgreSQL SQL to SQLite using sqlglot (Python subprocess).
///
/// Uses `unsupported_level=RAISE` so unsupported features surface as explicit
/// errors rather than silent best-effort passthroughs.
fn translate_sqlglot(sql: &str) -> String {
    use std::process::Command;
    let script = format!(
        concat!(
            "import sqlglot, sys\n",
            "try:\n",
            "    results = sqlglot.transpile({:?}, read='postgres', write='sqlite',",
            " unsupported_level=sqlglot.ErrorLevel.RAISE)\n",
            "    print('; '.join(results))\n",
            "except sqlglot.errors.UnsupportedError as e:\n",
            "    print('UNSUPPORTED: ' + str(e))\n",
            "except Exception as e:\n",
            "    print('ERROR: ' + str(e))\n",
        ),
        sql
    );
    let out = match Command::new("python3").arg("-c").arg(&script).output() {
        Ok(o) => o,
        Err(e) => return format!("ERROR: {e}"),
    };
    // sqlglot prints to stdout regardless of success/error path above.
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return format!("ERROR: {stderr}");
    }
    stdout
}

// ── Runtime execution ────────────────────────────────────────────────────────

/// Run `sql` against a fresh in-memory SQLite database.
///
/// Returns `"✓"` on success, `SKIPPED` if the translation already errored
/// (nothing to test), or `"✗ <short error>"` on failure.
fn try_sqlite(sql: &str) -> String {
    if sql.starts_with("ERROR:")
        || sql.starts_with("PARSE ERROR:")
        || sql.starts_with("UNSUPPORTED:")
        || sql == SKIPPED
    {
        return SKIPPED.to_string();
    }

    let Ok(conn) = Connection::open_in_memory() else {
        return SKIPPED.to_string();
    };

    // Detect CREATE TABLE as first statement — pre-populating would create a
    // conflicting `t`.  For every other form (SELECT, UPDATE, DELETE, CREATE
    // VIEW, CREATE TRIGGER, CREATE FUNCTION, …) we seed the catch-all tables
    // so the statement has something to reference.
    let mut words = sql.split_whitespace();
    let first = words.next().unwrap_or("").to_ascii_uppercase();
    let second = words.next().unwrap_or("").to_ascii_uppercase();
    let is_create_table = first == "CREATE" && second == "TABLE";

    if !is_create_table {
        let _ = conn.execute_batch(CATCH_ALL_SCHEMA);
    }

    match conn.execute_batch(sql) {
        Ok(()) => "\u{2713}".to_string(),
        Err(e) => {
            // Trim to keep Markdown table readable.
            let msg = e.to_string();
            let short = msg.lines().next().unwrap_or(&msg);
            let short = if short.len() > 60 { &short[..60] } else { short };
            format!("\u{2717} {short}")
        }
    }
}

// ── Reverse translators ──────────────────────────────────────────────────────

/// Reverse-translate a pg2sqlite SQLite output back to PostgreSQL using
/// pg2sqlite's own reverse translator, with a schema derived from the
/// original PG source SQL.
///
/// Returns `SKIPPED` when the forward translation already failed (nothing to
/// reverse), or `"ERROR: …"` / the recovered PG SQL string.
fn reverse_pg2sqlite(pg_sql: &str, sqlite_sql: &str) -> String {
    if sqlite_sql.starts_with("ERROR:") || sqlite_sql.starts_with("PARSE ERROR:") {
        return SKIPPED.to_string();
    }
    // Build the schema from the original PG source so that type information
    // (UUIDs, RLS tables, …) is available during reverse translation.
    let schema = match Pg2Sqlite::default().sql(pg_sql) {
        Ok(t) => {
            match t.build_schema() {
                Ok(s) => s,
                Err(_) => return SKIPPED.to_string(),
            }
        }
        Err(_) => return SKIPPED.to_string(),
    };
    let rt = Pg2Sqlite::default();
    match rt.reverse_sql(sqlite_sql, &schema, &Pg2SqliteOptions::default()) {
        Ok(stmts) => stmts.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("; "),
        Err(e) => format!("ERROR: {e}"),
    }
}

/// Reverse-translate SQLite SQL back to PostgreSQL using sqlglot (Python
/// subprocess).
fn reverse_sqlglot(sqlite_sql: &str) -> String {
    if sqlite_sql.starts_with("ERROR:")
        || sqlite_sql.starts_with("PARSE ERROR:")
        || sqlite_sql.starts_with("UNSUPPORTED:")
        || sqlite_sql == SKIPPED
    {
        return SKIPPED.to_string();
    }
    use std::process::Command;
    let script = format!(
        concat!(
            "import sqlglot, sys\n",
            "try:\n",
            "    results = sqlglot.transpile({:?}, read='sqlite', write='postgres')\n",
            "    print('; '.join(results))\n",
            "except Exception as e:\n",
            "    print('ERROR: ' + str(e))\n",
        ),
        sqlite_sql
    );
    let out = match Command::new("python3").arg("-c").arg(&script).output() {
        Ok(o) => o,
        Err(e) => return format!("ERROR: {e}"),
    };
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() {
        return format!("ERROR: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    stdout
}

/// Reverse-translate SQLite SQL back to PostgreSQL using polyglot-sql.
fn reverse_polyglot(sqlite_sql: &str) -> String {
    if sqlite_sql.starts_with("ERROR:") || sqlite_sql.starts_with("PARSE ERROR:") {
        return SKIPPED.to_string();
    }
    match transpile(sqlite_sql, DialectType::SQLite, DialectType::PostgreSQL) {
        Ok(stmts) => stmts.join("; "),
        Err(e) => format!("ERROR: {e}"),
    }
}

// ── Markdown helpers ─────────────────────────────────────────────────────────

/// Escape pipe characters so they don't break Markdown table cell boundaries.
fn escape_md(s: &str) -> String {
    s.replace('|', r"\|")
}

/// Compute all nine translation and execution results for a single test case.
fn compute(case: &Case) -> TranslationRow {
    let pg2s = translate_pg2sqlite(case.sql);
    let poly = translate_polyglot(case.sql);
    let sqlg = translate_sqlglot(case.sql);
    let pg2s_ok = try_sqlite(&pg2s);
    let poly_ok = try_sqlite(&poly);
    let sqlg_ok = try_sqlite(&sqlg);
    let pg2s_rt = reverse_pg2sqlite(case.sql, &pg2s);
    let poly_rev = reverse_polyglot(&pg2s);
    let sqlg_rev = reverse_sqlglot(&pg2s);
    TranslationRow { pg2s, poly, sqlg, pg2s_ok, poly_ok, sqlg_ok, pg2s_rt, poly_rev, sqlg_rev }
}

/// Print the Markdown `##` heading, blank line, column header, and separator
/// for a new category section.
fn print_category_header(category: &str) {
    println!("## {category}");
    println!();
    println!(
        "| Case | Input SQL | pg2s\u{2192}SQLite | pg2s runs? | pg2s RT\u{21a9}PG | poly\u{2192}SQLite | poly runs? | poly\u{21a9}PG (pg2s out) | sqlg\u{2192}SQLite | sqlg runs? | sqlg\u{21a9}PG (pg2s out) |"
    );
    println!(
        "|------|-----------|-------------|-----------|-----------|-------------|-----------|-------------------|-------------|-----------|-------------------|"
    );
}

/// Print a single Markdown table row for one test case.
fn print_row(case: &Case, row: &TranslationRow) {
    println!(
        "| {} | `{}` | `{}` | {} | `{}` | `{}` | {} | `{}` | `{}` | {} | `{}` |",
        case.name,
        escape_md(case.sql),
        escape_md(&row.pg2s),
        row.pg2s_ok,
        escape_md(&row.pg2s_rt),
        escape_md(&row.poly),
        row.poly_ok,
        escape_md(&row.poly_rev),
        escape_md(&row.sqlg),
        row.sqlg_ok,
        escape_md(&row.sqlg_rev),
    );
}

// ── Entry point ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
fn main() {
    // Print the bundled SQLite version so the reader knows what "✓" means.
    let sqlite_ver: String = {
        let conn = Connection::open_in_memory().unwrap();
        conn.query_row("SELECT sqlite_version()", [], |r| r.get(0)).unwrap()
    };
    println!("# pg2sqlite vs polyglot-sql: PostgreSQL \u{2192} SQLite Translation Comparison");
    println!();
    println!("*Generated by `cargo run --example compare_polyglot`*");
    println!();
    println!("SQLite version used for runtime checks: **{sqlite_ver}**");
    println!();
    println!(
        "> **SQLite?** column: \u{2713} = accepted and executed, \u{2717} = runtime error, \u{2014} = translation already failed (nothing to test)"
    );
    println!(
        "> **\u{21a9} PG** columns: reverse-translation of pg2sqlite\u{2019}s SQLite output back to PostgreSQL (pg2sqlite RT = pg2sqlite\u{2019}s own reverse translator; poly \u{21a9} PG = polyglot SQLite\u{2192}PG; sqlg \u{21a9} PG = sqlglot SQLite\u{2192}PG)"
    );
    println!();

    let cases: Vec<Case> = vec![
        // ── Category A: Simple Function Renames ──────────────────────────────
        Case::new("A: Simple Function Renames", "A1 LEAST", "SELECT LEAST(a, b) FROM t"),
        Case::new("A: Simple Function Renames", "A2 GREATEST", "SELECT GREATEST(a, b) FROM t"),
        Case::new("A: Simple Function Renames", "A3 STRPOS", "SELECT STRPOS(str, 'x') FROM t"),
        Case::new("A: Simple Function Renames", "A4 CHAR_LENGTH", "SELECT CHAR_LENGTH(str) FROM t"),
        Case::new(
            "A: Simple Function Renames",
            "A5 STRING_AGG",
            "SELECT STRING_AGG(name, ',') FROM t GROUP BY x",
        ),
        // ── Category B: NULL / Semantic Differences in Functions ─────────────
        Case::new(
            "B: NULL / Semantic Differences",
            "B1 CONCAT COALESCE",
            "SELECT CONCAT(a, b, c) FROM t",
        ),
        Case::new("B: NULL / Semantic Differences", "B2 NOW", "SELECT NOW()"),
        Case::new(
            "B: NULL / Semantic Differences",
            "B3 EXTRACT YEAR",
            "SELECT EXTRACT(YEAR FROM ts) FROM t",
        ),
        Case::new(
            "B: NULL / Semantic Differences",
            "B4 EXTRACT EPOCH",
            "SELECT EXTRACT(EPOCH FROM ts) FROM t",
        ),
        Case::new(
            "B: NULL / Semantic Differences",
            "B5 DATE_TRUNC month",
            "SELECT DATE_TRUNC('month', ts) FROM t",
        ),
        Case::new("B: NULL / Semantic Differences", "B6 FLOOR", "SELECT FLOOR(x) FROM t"),
        Case::new("B: NULL / Semantic Differences", "B7 CEIL", "SELECT CEIL(x) FROM t"),
        // ── Category C: Operators with Rewritten NULL Semantics ──────────────
        Case::new(
            "C: NULL-Safe Operators",
            "C1 IS DISTINCT FROM",
            "SELECT a IS DISTINCT FROM b FROM t",
        ),
        Case::new(
            "C: NULL-Safe Operators",
            "C2 IS NOT DISTINCT FROM",
            "SELECT a IS NOT DISTINCT FROM b FROM t",
        ),
        // ── Category D: String Syntax Sugar ──────────────────────────────────
        Case::new("D: String Syntax Sugar", "D1 POSITION", "SELECT POSITION('x' IN str) FROM t"),
        Case::new(
            "D: String Syntax Sugar",
            "D2 SUBSTRING FROM FOR",
            "SELECT SUBSTRING(str FROM 2 FOR 5) FROM t",
        ),
        Case::new(
            "D: String Syntax Sugar",
            "D3 TRIM LEADING",
            "SELECT TRIM(LEADING 'x' FROM str) FROM t",
        ),
        // ── Category E: AT TIME ZONE ─────────────────────────────────────────
        Case::new("E: AT TIME ZONE", "E1 offset", "SELECT ts AT TIME ZONE '+02:00' FROM t"),
        Case::new("E: AT TIME ZONE", "E2 named zone", "SELECT ts AT TIME ZONE 'utc' FROM t"),
        // ── Category F: DDL / Type Mapping ───────────────────────────────────
        Case::new(
            "F: DDL / Type Mapping",
            "F1 SERIAL BOOLEAN",
            "CREATE TABLE t (id SERIAL PRIMARY KEY, name TEXT, active BOOLEAN)",
        ),
        Case::new(
            "F: DDL / Type Mapping",
            "F2 JSONB TIMESTAMPTZ",
            "CREATE TABLE t (data JSONB, ts TIMESTAMPTZ)",
        ),
        Case::new("F: DDL / Type Mapping", "F3 UUID", "CREATE TABLE t (id UUID)"),
        Case::new(
            "F: DDL / Type Mapping",
            "F4 schema-qualified",
            "CREATE TABLE public.t (id INT, name TEXT)",
        ),
        // ── Category G: Aggregate FILTER Clause ──────────────────────────────
        Case::new(
            "G: Aggregate FILTER",
            "G1 COUNT FILTER",
            "SELECT COUNT(*) FILTER (WHERE x > 0) FROM t",
        ),
        Case::new(
            "G: Aggregate FILTER",
            "G2 SUM FILTER",
            "SELECT SUM(val) FILTER (WHERE active) FROM t",
        ),
        // ── Category H: JSON Aggregates ───────────────────────────────────────
        Case::new("H: JSON Aggregates", "H1 JSON_AGG", "SELECT JSON_AGG(name) FROM t"),
        Case::new(
            "H: JSON Aggregates",
            "H2 JSON_OBJECT_AGG",
            "SELECT JSON_OBJECT_AGG(k, v) FROM t",
        ),
        Case::new("H: JSON Aggregates", "H3 JSONB_AGG", "SELECT JSONB_AGG(name) FROM t"),
        // ── Category I: Unsupported PG Features → Explicit Errors ────────────
        Case::new(
            "I: Unsupported \u{2192} Explicit Error",
            "I1 BOOL_AND",
            "SELECT BOOL_AND(active) FROM t",
        ),
        Case::new("I: Unsupported \u{2192} Explicit Error", "I2 STDDEV", "SELECT STDDEV(x) FROM t"),
        Case::new(
            "I: Unsupported \u{2192} Explicit Error",
            "I3 ARRAY_AGG",
            "SELECT ARRAY_AGG(name) FROM t",
        ),
        Case::new(
            "I: Unsupported \u{2192} Explicit Error",
            "I4 SPLIT_PART",
            "SELECT SPLIT_PART(str, ',', 2) FROM t",
        ),
        Case::new(
            "I: Unsupported \u{2192} Explicit Error",
            "I5 REGEXP_REPLACE",
            "SELECT REGEXP_REPLACE(str, 'a', 'b') FROM t",
        ),
        Case::new(
            "I: Unsupported \u{2192} Explicit Error",
            "I6 TO_CHAR",
            "SELECT TO_CHAR(ts, 'YYYY-MM-DD') FROM t",
        ),
        Case::new(
            "I: Unsupported \u{2192} Explicit Error",
            "I7 PERCENTILE_CONT",
            "SELECT PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY val) FROM t",
        ),
        Case::new(
            "I: Unsupported \u{2192} Explicit Error",
            "I8 BIT_AND",
            "SELECT BIT_AND(x) FROM t",
        ),
        // ── Category J: pgvector Operators ───────────────────────────────────
        Case::new("J: pgvector Operators", "J1 <-> L2 distance", "SELECT a <-> b FROM t"),
        Case::new("J: pgvector Operators", "J2 <=> cosine distance", "SELECT a <=> b FROM t"),
        Case::new("J: pgvector Operators", "J3 <~> hamming distance", "SELECT a <~> b FROM t"),
        // ── Category K: Window Functions ──────────────────────────────────────
        Case::new(
            "K: Window Functions",
            "K1 SUM OVER DATE_TRUNC",
            "SELECT SUM(val) OVER (PARTITION BY DATE_TRUNC('month', created_at)) FROM t",
        ),
        Case::new(
            "K: Window Functions",
            "K2 ROW_NUMBER OVER NOW",
            "SELECT ROW_NUMBER() OVER (ORDER BY NOW()) FROM t",
        ),
        // ── Category L: DML Expression Translation ────────────────────────────
        Case::new(
            "L: DML Expression Translation",
            "L1 UPDATE SET NOW()",
            "UPDATE t SET updated_at = NOW() WHERE id = 1",
        ),
        Case::new(
            "L: DML Expression Translation",
            "L2 DELETE WHERE NOW()",
            "DELETE FROM events WHERE NOW() > created_at",
        ),
        // ── Category M: Row-Level Security ────────────────────────────────────
        Case::new(
            "M: Row-Level Security",
            "M1 Full RLS block",
            concat!(
                "CREATE TABLE t (id INT PRIMARY KEY, owner TEXT, val INT);",
                "ALTER TABLE t ENABLE ROW LEVEL SECURITY;",
                "CREATE POLICY owner_policy ON t USING (owner = current_user);"
            ),
        ),
        // ── Category N: PL/pgSQL Trigger translation ──────────────────────────
        Case::new(
            "N: PL/pgSQL Trigger",
            "N1 IF/ELSIF/ELSE",
            concat!(
                "CREATE TABLE t (id INT PRIMARY KEY, type TEXT, val INT); ",
                "CREATE TABLE log (id INT, val TEXT); ",
                "CREATE OR REPLACE FUNCTION trg_fn() RETURNS TRIGGER LANGUAGE plpgsql AS $$ ",
                "BEGIN ",
                "  IF NEW.type = 'a' THEN INSERT INTO log VALUES (NEW.id, 'type_a'); ",
                "  ELSIF NEW.type = 'b' THEN INSERT INTO log VALUES (NEW.id, 'type_b'); ",
                "  ELSE INSERT INTO log VALUES (NEW.id, 'other'); ",
                "  END IF; ",
                "  RETURN NEW; ",
                "END; $$; ",
                "CREATE TRIGGER trg AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION trg_fn();"
            ),
        ),
        Case::new(
            "N: PL/pgSQL Trigger",
            "N2 RAISE EXCEPTION",
            concat!(
                "CREATE TABLE t (id INT PRIMARY KEY, val INT); ",
                "CREATE OR REPLACE FUNCTION trg_fn() RETURNS TRIGGER LANGUAGE plpgsql AS $$ ",
                "BEGIN ",
                "  IF NEW.val < 0 THEN RAISE EXCEPTION 'val must be non-negative'; END IF; ",
                "  RETURN NEW; ",
                "END; $$; ",
                "CREATE TRIGGER trg_check BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION trg_fn();"
            ),
        ),
        Case::new(
            "N: PL/pgSQL Trigger",
            "N3 variable + SELECT INTO",
            concat!(
                "CREATE TABLE t (id INT PRIMARY KEY, val INT); ",
                "CREATE TABLE audit (id INT, old_val INT, new_val INT); ",
                "CREATE OR REPLACE FUNCTION trg_audit() RETURNS TRIGGER LANGUAGE plpgsql AS $$ ",
                "DECLARE prev_val INT; ",
                "BEGIN ",
                "  SELECT val INTO prev_val FROM t WHERE id = NEW.id; ",
                "  INSERT INTO audit VALUES (NEW.id, prev_val, NEW.val); ",
                "  RETURN NEW; ",
                "END; $$; ",
                "CREATE TRIGGER trg_audit AFTER UPDATE ON t FOR EACH ROW EXECUTE FUNCTION trg_audit();"
            ),
        ),
        // ── Category O: Things polyglot might do where pg2sqlite errors ───────
        Case::new(
            "O: polyglot-only candidates",
            "O1 ROLLUP",
            "SELECT department, SUM(salary) FROM emp GROUP BY ROLLUP(department)",
        ),
        Case::new(
            "O: polyglot-only candidates",
            "O2 ARRAY_AGG ORDER BY",
            "SELECT ARRAY_AGG(name ORDER BY name) FROM t",
        ),
        Case::new(
            "O: polyglot-only candidates",
            "O3 STDDEV formula",
            "SELECT STDDEV(salary) FROM emp",
        ),
        Case::new(
            "O: polyglot-only candidates",
            "O4 SIMILAR TO",
            "SELECT name FROM t WHERE name SIMILAR TO '%(cat|dog)%'",
        ),
        Case::new(
            "O: polyglot-only candidates",
            "O5 MATERIALIZED VIEW",
            "CREATE MATERIALIZED VIEW mv AS SELECT id, SUM(qty) FROM t GROUP BY id",
        ),
        Case::new(
            "O: polyglot-only candidates",
            "O6 CREATE OR REPLACE VIEW",
            "CREATE OR REPLACE VIEW v AS SELECT id FROM t",
        ),
        Case::new(
            "O: polyglot-only candidates",
            "O7 JSON_BUILD_OBJECT",
            "SELECT JSON_BUILD_OBJECT('id', id, 'name', name) FROM t",
        ),
        Case::new(
            "O: polyglot-only candidates",
            "O8 NULLIF passthrough",
            "SELECT NULLIF(a, b) FROM t",
        ),
        Case::new(
            "O: polyglot-only candidates",
            "O9 COALESCE passthrough",
            "SELECT COALESCE(a, b, c) FROM t",
        ),
        // ── Category P: JSON Operators & Constructors ─────────────────────────
        Case::new("P: JSON Operators & Constructors", "P1 -> field", "SELECT data->'name' FROM t"),
        Case::new(
            "P: JSON Operators & Constructors",
            "P2 ->> field",
            "SELECT data->>'name' FROM t",
        ),
        Case::new(
            "P: JSON Operators & Constructors",
            "P3 JSON_BUILD_OBJECT",
            "SELECT JSON_BUILD_OBJECT('id', id, 'name', name) FROM t",
        ),
        Case::new(
            "P: JSON Operators & Constructors",
            "P4 JSON_BUILD_ARRAY",
            "SELECT JSON_BUILD_ARRAY(a, b, c) FROM t",
        ),
        Case::new(
            "P: JSON Operators & Constructors",
            "P5 JSON_EXTRACT",
            "SELECT JSON_EXTRACT(data, '$.name') FROM t",
        ),
        // ── Category Q: More String Functions ────────────────────────────────
        Case::new("Q: More String Functions", "Q1 LPAD", "SELECT LPAD(str, 10, '0') FROM t"),
        Case::new("Q: More String Functions", "Q2 RPAD", "SELECT RPAD(str, 10, ' ') FROM t"),
        Case::new("Q: More String Functions", "Q3 INITCAP", "SELECT INITCAP(str) FROM t"),
        Case::new(
            "Q: More String Functions",
            "Q4 ILIKE",
            "SELECT name FROM t WHERE name ILIKE '%foo%'",
        ),
        Case::new(
            "Q: More String Functions",
            "Q5 OVERLAY",
            "SELECT OVERLAY(str PLACING 'x' FROM 1 FOR 1) FROM t",
        ),
        // ── Category R: Extended DDL Type Mappings ───────────────────────────
        Case::new(
            "R: Extended DDL Types",
            "R1 BIGINT NUMERIC DATE",
            "CREATE TABLE t (id BIGINT PRIMARY KEY, amount NUMERIC(10,2), dt DATE)",
        ),
        Case::new(
            "R: Extended DDL Types",
            "R2 BIT NVARCHAR TSVECTOR",
            "CREATE TABLE t (flag BIT, label NVARCHAR(100), doc TSVECTOR)",
        ),
        Case::new(
            "R: Extended DDL Types",
            "R3 BLOB precision",
            "SELECT CAST(id AS BLOB(50)) FROM t",
        ),
        // ── Category S: DML Extras ────────────────────────────────────────────
        Case::new(
            "S: DML Extras",
            "S1 ON CONFLICT DO NOTHING",
            "INSERT INTO t (id, name) VALUES (1, 'a') ON CONFLICT DO NOTHING",
        ),
        Case::new(
            "S: DML Extras",
            "S2 ON CONFLICT DO UPDATE",
            "INSERT INTO t (id, name) VALUES (1, 'a') ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
        ),
        Case::new("S: DML Extras", "S3 RETURNING", "INSERT INTO t (id) VALUES (1) RETURNING id"),
        // ── Category T: More Date/Time Functions ──────────────────────────────
        Case::new("T: More Date/Time", "T1 date_part year", "SELECT date_part('year', ts) FROM t"),
        Case::new(
            "T: More Date/Time",
            "T2 date_part epoch",
            "SELECT date_part('epoch', ts) FROM t",
        ),
        Case::new("T: More Date/Time", "T3 DATE typed literal", "SELECT DATE '2023-01-01'"),
        Case::new(
            "T: More Date/Time",
            "T4 TO_CHAR date",
            "SELECT TO_CHAR(ts, 'YYYY-MM-DD') FROM t",
        ),
        // ── Category U: PostgreSQL-Specific Idioms ────────────────────────────
        Case::new("U: PG-Specific Idioms", "U1 nextval", "SELECT nextval('my_seq')"),
        Case::new(
            "U: PG-Specific Idioms",
            "U2 generate_series",
            "SELECT * FROM generate_series(1, 10)",
        ),
        Case::new(
            "U: PG-Specific Idioms",
            "U3 NULL col option",
            "CREATE TABLE t (id INT, col TEXT NULL)",
        ),
        // ── Category V: Roles & Permissions ──────────────────────────────────
        Case::new("V: Roles & Permissions", "V1 CREATE ROLE", "CREATE ROLE app_user"),
        Case::new("V: Roles & Permissions", "V2 GRANT SELECT", "GRANT SELECT ON t TO app_user"),
        Case::new("V: Roles & Permissions", "V3 GRANT ALL", "GRANT ALL ON t TO app_user"),
        Case::new("V: Roles & Permissions", "V4 REVOKE", "REVOKE SELECT ON t FROM app_user"),
        // ── Category W: GIN / Full-Text Search Indices ────────────────────────
        Case::new(
            "W: GIN / FTS Indices",
            "W1 GIN tsvector index",
            concat!(
                "CREATE TABLE docs (id INT PRIMARY KEY, body TEXT);",
                "CREATE INDEX docs_fts ON docs USING GIN (to_tsvector('english', body));",
            ),
        ),
        Case::new(
            "W: GIN / FTS Indices",
            "W2 GIN jsonb index",
            "CREATE TABLE docs (id INT PRIMARY KEY, data JSONB); CREATE INDEX docs_gin ON docs USING GIN (data);",
        ),
        Case::new(
            "W: GIN / FTS Indices",
            "W3 GiST index",
            concat!(
                "CREATE TABLE docs (id INT PRIMARY KEY, body TEXT);",
                "CREATE INDEX docs_gist ON docs USING GIST (to_tsvector('english', body));",
            ),
        ),
        Case::new(
            "W: GIN / FTS Indices",
            "W4 plain BTREE index",
            "CREATE TABLE t2 (id INT PRIMARY KEY, name TEXT); CREATE INDEX t2_name ON t2 (name);",
        ),
    ];

    let mut current_cat = "";
    for case in &cases {
        if case.category != current_cat {
            if !current_cat.is_empty() {
                println!();
            }
            print_category_header(case.category);
            current_cat = case.category;
        }
        let row = compute(case);
        print_row(case, &row);
    }

    println!();
}
