//! A long left-deep chain translates on an ordinary thread instead of
//! overflowing its stack, which aborts the process.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use run_translated_helper::run_translated_with;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

/// Past the 13 levels a debug build and the 74 a release build survived, and
/// short of where `sqlparser`'s derived `Clone` aborts in a debug build.
const OPERANDS: usize = 150;

/// Past the 15 arms a debug build survived, and short of where `sqlparser`
/// clones a `SetExpr` chain into an abort in a debug build.
const ARMS: usize = 80;

const ROWS: &str = "CREATE TABLE t (a int, b text); INSERT INTO t VALUES (1, 'x');";

/// Runs `f` on a thread with the 2 MiB stack a spawned Rust thread gets by
/// default.
fn on_a_default_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("the translation thread")
}

fn chain(term: &str, separator: &str, length: usize) -> String {
    vec![term; length].join(separator)
}

/// The single value the last statement of `pg` answers in SQLite.
fn answer(pg: String) -> Option<String> {
    on_a_default_thread(move || {
        let mut rows = run_translated_with(&pg, &Pg2SqliteOptions::default());
        assert_eq!(rows.len(), 1, "one row");
        rows.remove(0)
    })
}

#[test]
fn a_long_arithmetic_chain_translates() {
    let pg = format!("{ROWS} SELECT {} FROM t;", chain("a", " + ", OPERANDS));
    assert_eq!(answer(pg), Some(OPERANDS.to_string()));
}

#[test]
fn a_long_boolean_chain_translates() {
    let pg = format!("{ROWS} SELECT count(*) FROM t WHERE {};", chain("a > 0", " AND ", OPERANDS));
    assert_eq!(answer(pg), Some("1".to_string()));
}

#[test]
fn a_long_concatenation_translates() {
    let pg = format!("{ROWS} SELECT length({}) FROM t;", chain("b", " || ", OPERANDS));
    assert_eq!(answer(pg), Some(OPERANDS.to_string()));
}

#[test]
fn a_long_set_operation_chain_translates() {
    let pg = format!(
        "{ROWS} SELECT count(*) FROM ({}) AS u;",
        chain("SELECT a FROM t", " UNION ALL ", ARMS)
    );
    assert_eq!(answer(pg), Some(ARMS.to_string()));
}

#[test]
fn a_long_arithmetic_chain_reverses() {
    let sqlite = format!("SELECT {} FROM t", chain("a", " + ", OPERANDS));
    let postgres = on_a_default_thread(move || {
        let translator = Pg2Sqlite::default().sql("CREATE TABLE t (a int, b text);").unwrap();
        let schema = translator.build_schema().unwrap();
        let statements =
            translator.reverse_sql(&sqlite, &schema, &Pg2SqliteOptions::default()).unwrap();
        statements.iter().map(ToString::to_string).collect::<Vec<_>>().join(";")
    });
    assert_eq!(postgres.matches(" + ").count(), OPERANDS - 1, "{postgres}");
    Parser::parse_sql(&PostgreSqlDialect {}, &postgres).expect("reverse output reparses");
}
