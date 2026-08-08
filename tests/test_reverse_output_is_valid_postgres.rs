//! The reverse direction's guarantee: SQLite in, valid PostgreSQL out.
//!
//! `Pg2Sqlite::reverse_sql` exists so a SQLite replica's DML can be replayed
//! against the PostgreSQL backend it came from. That only works if the output
//! is PostgreSQL a server will accept, so a SQLite-only construct must be
//! translated or rejected, never passed through.
//!
//! Each case states its expected outcome. `Rejected` means the construct has no
//! PostgreSQL form. `Emits` names the substring the output must contain, chosen
//! to pin the construct that matters rather than the whole statement.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;

/// What reverse translation must do with a construct.
enum Expect {
    /// Output must contain this substring.
    Emits(&'static str),
    /// Translation must return `Err`.
    Rejected,
}
use Expect::{Emits, Rejected};

fn schema() -> ParserDB {
    Pg2Sqlite::default()
        .sql("CREATE TABLE t (id INT PRIMARY KEY, s TEXT, n INT, r REAL, payload JSONB);")
        .expect("fixture parses")
        .build_schema()
        .expect("fixture builds a schema")
}

/// Spellings SQLite has and PostgreSQL does not. Any of these surviving into
/// the output means the construct was passed through rather than translated.
/// This is a backstop for constructs the per-case list has not reached yet.
const SQLITE_ONLY: &[&str] = &[
    "ifnull(",
    "iif(",
    "total(",
    "hex(",
    "typeof(",
    "printf(",
    "randomblob(",
    "changes(",
    "last_insert_rowid(",
    "rowid",
    " GLOB ",
    "julianday(",
    "unixepoch(",
    "sqlite_",
    "OR ROLLBACK",
    "OR ABORT",
    "OR FAIL",
    "OR REPLACE",
    "OR IGNORE",
    "json_set(",
    "json_insert(",
    "json_remove(",
    "json_patch(",
    "json_quote(",
    "json_valid(",
    "json_extract(",
    "json_group_array(",
    "json_group_object(",
    "group_concat(",
    "instr(",
    "strftime(",
    "vec_",
];

/// True when `needle` occurs in `haystack` as its own identifier, so that
/// `typeof(` does not match inside `json_typeof(` nor `rowid` inside
/// `last_insert_rowid`.
fn contains_bare(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(at, _)| {
        haystack[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric() && c != '_')
    })
}

fn check(cases: &[(&str, Expect)]) {
    let schema = schema();
    let options = Pg2SqliteOptions::default();
    let dialect = sqlparser::dialect::PostgreSqlDialect {};
    let mut failures = Vec::new();

    for (sqlite, expect) in cases {
        let outcome = Pg2Sqlite::default().reverse_sql(sqlite, &schema, &options);
        match (outcome, expect) {
            (Err(_), Rejected) => {}
            (Ok(stmts), Rejected) => {
                let out = stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
                failures.push(format!("{sqlite}\n      expected Err, got: {out}"));
            }
            (Err(e), Emits(want)) => {
                failures.push(format!(
                    "{sqlite}\n      expected output containing {want:?}, got Err: {e}"
                ));
            }
            (Ok(stmts), Emits(want)) => {
                let out = stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
                if !out.contains(want) {
                    failures.push(format!("{sqlite}\n      expected {want:?} in: {out}"));
                    continue;
                }
                if sqlparser::parser::Parser::parse_sql(&dialect, &out).is_err() {
                    failures
                        .push(format!("{sqlite}\n      output is not PostgreSQL syntax: {out}"));
                    continue;
                }
                if let Some(leak) = SQLITE_ONLY.iter().find(|k| contains_bare(&out, k)) {
                    failures.push(format!(
                        "{sqlite}\n      SQLite-only spelling {leak:?} survived into: {out}"
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} reverse translation(s) wrong:\n\n{}\n",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn json_functions_reverse_to_postgres_spellings() {
    check(&[
        ("SELECT json(s) FROM t", Emits("JSONB")),
        ("SELECT json_set(payload, '$.a', 1) FROM t", Emits("jsonb_set")),
        ("SELECT json_insert(payload, '$.a', 1) FROM t", Emits("jsonb_insert")),
        ("SELECT json_remove(payload, '$.a') FROM t", Emits("#-")),
        ("SELECT json_quote(s) FROM t", Emits("to_jsonb")),
        ("SELECT json_valid(s) FROM t", Emits("IS JSON")),
        ("SELECT json_extract(payload, '$.a') FROM t", Emits("#>")),
        ("SELECT json_patch(payload, payload) FROM t", Emits("||")),
        // Already correct, kept so a regression shows up here.
        ("SELECT json_type(payload) FROM t", Emits("json_typeof")),
        ("SELECT json_array_length(payload) FROM t", Emits("jsonb_array_length")),
        ("SELECT json_group_array(s) FROM t", Emits("json_agg")),
        ("SELECT json_array(s) FROM t", Emits("json_build_array")),
    ]);
}

#[test]
fn scalar_functions_reverse_to_postgres_spellings() {
    check(&[
        ("SELECT ifnull(n, 0) FROM t", Emits("COALESCE")),
        ("SELECT iif(n > 0, 1, 0) FROM t", Emits("CASE")),
        ("SELECT total(n) FROM t", Emits("COALESCE")),
        ("SELECT hex(s) FROM t", Emits("encode")),
        ("SELECT unhex(s) FROM t", Emits("decode")),
        ("SELECT unixepoch(s) FROM t", Emits("EXTRACT")),
        // Already correct.
        ("SELECT group_concat(s, ',') FROM t", Emits("string_agg(s, ',')")),
        // PostgreSQL has no one-argument string_agg, so the comma SQLite
        // joins with is written out.
        ("SELECT group_concat(s) FROM t", Emits("string_agg(s, ',')")),
        ("SELECT group_concat(DISTINCT s) FROM t", Emits("string_agg(DISTINCT s, ',')")),
        ("SELECT instr(s, 'a') FROM t", Emits("POSITION")),
        ("SELECT unicode(s) FROM t", Emits("ascii")),
        ("SELECT min(n, 1) FROM t", Emits("LEAST")),
        ("SELECT max(n, 1) FROM t", Emits("GREATEST")),
        ("SELECT nullif(n, 0) FROM t", Emits("nullif")),
    ]);
}

#[test]
fn sqlite_only_functions_are_rejected() {
    check(&[
        ("SELECT typeof(n) FROM t", Rejected),
        ("SELECT printf('%d', n) FROM t", Rejected),
        ("SELECT randomblob(4) FROM t", Rejected),
        ("SELECT changes() FROM t", Rejected),
        ("SELECT last_insert_rowid() FROM t", Rejected),
        ("SELECT rowid FROM t", Rejected),
        ("SELECT julianday(s) FROM t", Rejected),
        ("SELECT random() FROM t", Rejected),
    ]);
}

/// PostgreSQL has no `strftime`. Three tables decide what one becomes, and
/// every format outside them is refused rather than forwarded.
#[test]
fn strftime_becomes_a_postgres_spelling_or_nothing() {
    check(&[
        ("SELECT strftime('%Y-01-01 00:00:00', s) FROM t", Emits("date_trunc('year', s)")),
        ("SELECT strftime('%Y', s) FROM t", Emits("EXTRACT(YEAR FROM s)")),
        ("SELECT strftime('%Y-%m-%d', s) FROM t", Emits("to_char(s, 'YYYY-MM-DD')")),
        ("SELECT strftime('%H:%M:%S', s) FROM t", Emits("to_char(s, 'HH24:MI:SS')")),
        // PostgreSQL reads a bare template `T` as the start of `TH` or `TM`.
        ("SELECT strftime('%Y-%m-%dT%H', s) FROM t", Emits("to_char(s, 'YYYY-MM-DD\"T\"HH24')")),
        // The Sunday based week, which no PostgreSQL field matches.
        ("SELECT strftime('%W', s) FROM t", Rejected),
        // SQLite has no `%y`, so the call answers NULL and names nothing.
        ("SELECT strftime('%y', s) FROM t", Rejected),
        // SQLite's trailing date modifiers have no PostgreSQL form.
        ("SELECT strftime('%Y', s, 'utc') FROM t", Rejected),
        ("SELECT strftime(s, s) FROM t", Rejected),
    ]);
}

#[test]
fn glob_has_no_postgres_operator() {
    check(&[
        // A literal pattern converts to LIKE: GLOB's `*` and `?` become `%`
        // and `_`, and LIKE's own wildcards are escaped.
        ("SELECT s FROM t WHERE s GLOB 'a*'", Emits("LIKE 'a%'")),
        ("SELECT s FROM t WHERE s GLOB 'a?b'", Emits("LIKE 'a_b'")),
        // A computed pattern cannot be converted at translation time.
        ("SELECT s FROM t WHERE s GLOB s", Rejected),
    ]);
}

/// `~` is PostgreSQL's case-sensitive POSIX regex operator. Measured against
/// both engines with `^[A-Z]`: SQLite `REGEXP` with the usual host-registered
/// function and PostgreSQL `~` agree on `'A'` and on `'a'`, and the negations
/// agree too. `RLIKE` is not SQLite at all, `near "RLIKE": syntax error`, so
/// it cannot have arrived from a SQLite replica.
#[test]
fn regexp_becomes_the_posix_operator() {
    check(&[
        ("SELECT s FROM t WHERE s REGEXP '^[A-Z]'", Emits("~ '^[A-Z]'")),
        ("SELECT s FROM t WHERE s NOT REGEXP '^[A-Z]'", Emits("!~ '^[A-Z]'")),
        ("SELECT s FROM t WHERE s RLIKE '^[A-Z]'", Rejected),
        ("SELECT s FROM t WHERE s NOT RLIKE '^[A-Z]'", Rejected),
    ]);
}

/// `INSERT OR REPLACE` deletes the conflicting row and inserts the new one, so
/// a column left out of the insert comes back as its default rather than
/// keeping the old value. Reversing a partial column list to `DO NOTHING`
/// preserves the old row instead, which silently discards the replica's write
/// when it is replayed upstream.
#[test]
fn insert_or_replace_with_a_partial_column_list_resets_the_other_columns() {
    check(&[
        ("INSERT OR REPLACE INTO t (id) VALUES (1)", Emits("DO UPDATE SET")),
        ("REPLACE INTO t (id) VALUES (1)", Emits("DO UPDATE SET")),
    ]);
}

#[test]
fn insert_or_replace_reverses_to_an_upsert_not_a_no_op() {
    check(&[
        ("INSERT OR REPLACE INTO t (id, s) VALUES (1, 'x')", Emits("DO UPDATE SET")),
        ("REPLACE INTO t (id, s) VALUES (1, 'x')", Emits("DO UPDATE SET")),
    ]);
}

#[test]
fn insert_or_replace_assigns_from_excluded() {
    let out = Pg2Sqlite::default()
        .reverse_sql(
            "INSERT OR REPLACE INTO t (id, s, n) VALUES (1, 'x', 2)",
            &schema(),
            &Pg2SqliteOptions::default(),
        )
        .expect("reverse translates")
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    assert!(out.contains("ON CONFLICT(id)"), "conflict target is the primary key: {out}");
    assert!(out.contains("s = EXCLUDED.s"), "every non-key column is overwritten: {out}");
    assert!(out.contains("n = EXCLUDED.n"), "every non-key column is overwritten: {out}");
    sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::PostgreSqlDialect {}, &out)
        .expect("reverse output must parse as PostgreSQL");
}

#[test]
fn insert_or_ignore_still_reverses_to_do_nothing() {
    check(&[("INSERT OR IGNORE INTO t (id) VALUES (1)", Emits("DO NOTHING"))]);
}

#[test]
fn update_conflict_clauses_have_no_postgres_form() {
    check(&[
        ("UPDATE OR ROLLBACK t SET n = 1", Rejected),
        ("UPDATE OR ABORT t SET n = 1", Rejected),
        ("UPDATE OR FAIL t SET n = 1", Rejected),
    ]);
}

#[test]
fn fts_match_has_no_bare_postgres_operator() {
    check(&[("SELECT n FROM t WHERE t MATCH 'x'", Rejected)]);
}
