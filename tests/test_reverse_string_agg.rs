//! F19: SQLite's one-argument `group_concat` has no one-argument counterpart.
//!
//! `group_concat(x)` joins with a comma. PostgreSQL's `string_agg` requires the
//! separator: `string_agg(x)` and `string_agg(DISTINCT x)` both answer
//! `function string_agg(text) does not exist` on PostgreSQL 17. So the reverse
//! direction writes the comma out rather than dropping the argument count.
//!
//! This is a round trip the crate breaks against itself. The forward direction
//! turns `string_agg(DISTINCT x, ',')` into `group_concat(DISTINCT x)`, because
//! SQLite refuses a separator beside DISTINCT, so the one-argument spelling is
//! output the crate produces and could not read back.
//!
//! The assertions pin the emitted call text, which is what was measured against
//! PostgreSQL 17 before the fix. Parsing alone does not settle it: the broken
//! output parsed perfectly well and simply named a function that is not there.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

const FIXTURE: &str = "CREATE TABLE tags (id INT PRIMARY KEY, name TEXT, category TEXT);";

fn schema() -> ParserDB {
    Pg2Sqlite::default().sql(FIXTURE).expect("parse").build_schema().expect("build")
}

/// Reverse-translates `sqlite` and returns the PostgreSQL text, having checked
/// it parses as PostgreSQL.
fn reverse(sqlite: &str) -> String {
    let statements = Pg2Sqlite::default()
        .reverse_sql(sqlite, &schema(), &Pg2SqliteOptions::default())
        .expect("reverse translation");
    let sql = statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    Parser::parse_sql(&PostgreSqlDialect {}, &sql).expect("output parses as PostgreSQL");
    sql
}

/// Forward-translates `pg` and returns the emitted SQLite text.
fn forward(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(&format!("{FIXTURE}\n{pg}"))
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .pop()
        .expect("at least one statement")
}

#[test]
fn a_one_argument_group_concat_gains_the_default_separator() {
    let sql = reverse("SELECT group_concat(name) FROM tags");
    assert!(sql.contains("string_agg(name, ',')"), "{sql}");
}

#[test]
fn a_two_argument_group_concat_keeps_its_own_separator() {
    let sql = reverse("SELECT group_concat(name, '|') FROM tags");
    assert!(sql.contains("string_agg(name, '|')"), "{sql}");
}

#[test]
fn a_distinct_group_concat_gains_the_default_separator() {
    let sql = reverse("SELECT group_concat(DISTINCT name) FROM tags");
    assert!(sql.contains("string_agg(DISTINCT name, ',')"), "{sql}");
}

#[test]
fn an_ordered_group_concat_gains_the_default_separator() {
    let sql = reverse("SELECT group_concat(name ORDER BY name DESC) FROM tags");
    assert!(sql.contains("string_agg(name, ',' ORDER BY name DESC)"), "{sql}");
}

#[test]
fn a_windowed_group_concat_gains_the_default_separator() {
    let sql = reverse("SELECT group_concat(name) OVER (ORDER BY id) FROM tags");
    assert!(sql.contains("string_agg(name, ',') OVER (ORDER BY id)"), "{sql}");
}

#[test]
fn a_filtered_group_concat_gains_the_default_separator() {
    let sql = reverse("SELECT group_concat(name) FILTER (WHERE id > 1) FROM tags");
    assert!(sql.contains("string_agg(name, ',')"), "{sql}");
}

/// The forward direction drops the separator beside DISTINCT because SQLite
/// refuses it there, so this is the crate reading back its own output.
#[test]
fn the_crate_reads_back_its_own_distinct_aggregate() {
    let sqlite = forward("SELECT string_agg(DISTINCT name, ',') FROM tags;");
    assert!(sqlite.contains("group_concat(DISTINCT name)"), "{sqlite}");
    let restored = reverse(&sqlite);
    assert!(restored.contains("string_agg(DISTINCT name, ',')"), "{restored}");
}

/// A separator the forward direction did carry through survives both ways.
#[test]
fn a_separated_aggregate_round_trips_unchanged() {
    let sqlite = forward("SELECT string_agg(name, '|') FROM tags;");
    assert!(sqlite.contains("group_concat(name, '|')"), "{sqlite}");
    let restored = reverse(&sqlite);
    assert!(restored.contains("string_agg(name, '|')"), "{restored}");
}
