//! Where a statement sits relative to a transaction block decides what
//! PostgreSQL does with it, and the translation ignored the question.
//!
//! A second `BEGIN` and an unmatched `COMMIT` are warnings the server shrugs
//! off, and both hard-failed the emitted script. A savepoint outside a
//! transaction block and `CREATE INDEX CONCURRENTLY` inside one are errors the
//! server refuses, and both went through. `SET TRANSACTION` was refused as
//! though it were a configuration parameter deciding name resolution.
//!
//! Every expected value is what PostgreSQL 17.3 answers, measured.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    warnings::TranslationWarning,
};
use run_translated_helper::run_translated_with;

const TABLE: &str = "CREATE TABLE t (a int);";

/// The rows `pg` answers.
fn answer(pg: &str) -> Vec<Option<String>> {
    run_translated_with(pg, &Pg2SqliteOptions::default())
}

/// The refusal `pg` earns.
fn refusal(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("expected a refusal")
        .to_string()
}

/// The warnings `pg` emits.
fn warnings(pg: &str) -> Vec<TranslationWarning> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("fixture parses")
        .translate_with_report(&Pg2SqliteOptions::default())
        .expect("fixture translates")
        .warnings
}

/// Whether any warning names `construct`.
fn warned_about(pg: &str, construct: &str) -> bool {
    warnings(pg).iter().any(|warning| format!("{warning:?}").contains(construct))
}

#[test]
fn a_second_begin_inside_a_transaction_is_dropped() {
    // PostgreSQL: WARNING: there is already a transaction in progress, and the
    // insert commits, so the count is 1. SQLite failed the script at the
    // second BEGIN with `cannot start a transaction within a transaction`.
    assert_eq!(
        answer(&format!(
            "{TABLE} BEGIN; BEGIN; INSERT INTO t VALUES (1); COMMIT; SELECT count(*) FROM t;"
        )),
        vec![Some("1".to_string())]
    );
    assert!(
        warned_about(&format!("{TABLE} BEGIN; BEGIN; INSERT INTO t VALUES (1); COMMIT;"), "BEGIN"),
        "dropping the redundant BEGIN is reported"
    );
}

#[test]
fn a_commit_with_no_transaction_open_is_dropped() {
    // PostgreSQL: WARNING: there is no transaction in progress, and the script
    // continues. SQLite failed with `cannot commit - no transaction is
    // active`, taking every later statement with it.
    assert_eq!(
        answer(&format!("{TABLE} INSERT INTO t VALUES (1); COMMIT; SELECT count(*) FROM t;")),
        vec![Some("1".to_string())]
    );
    assert!(
        warned_about(&format!("{TABLE} INSERT INTO t VALUES (1); COMMIT;"), "COMMIT"),
        "dropping the unmatched COMMIT is reported"
    );
}

#[test]
fn a_rollback_with_no_transaction_open_is_dropped() {
    // Same warning on the server, and `cannot rollback - no transaction is
    // active` in SQLite.
    assert_eq!(
        answer(&format!(
            "{TABLE} BEGIN; INSERT INTO t VALUES (1); COMMIT; ROLLBACK; \
             SELECT count(*) FROM t;"
        )),
        vec![Some("1".to_string())]
    );
}

#[test]
fn a_matched_commit_still_commits() {
    assert_eq!(
        answer(&format!(
            "{TABLE} BEGIN; INSERT INTO t VALUES (1); COMMIT; SELECT count(*) FROM t;"
        )),
        vec![Some("1".to_string())]
    );
}

#[test]
fn a_rollback_inside_a_transaction_still_undoes_the_write() {
    assert_eq!(
        answer(&format!(
            "{TABLE} BEGIN; INSERT INTO t VALUES (1); ROLLBACK; SELECT count(*) FROM t;"
        )),
        vec![Some("0".to_string())]
    );
}

#[test]
fn a_savepoint_outside_a_transaction_block_is_refused() {
    // PostgreSQL: ERROR: SAVEPOINT can only be used in transaction blocks.
    // SQLite opened an implicit transaction and the following write committed.
    let message = refusal(&format!("{TABLE} SAVEPOINT z; INSERT INTO t VALUES (1);"));
    assert!(message.contains("SAVEPOINT"), "{message}");
    assert!(message.contains("transaction block"), "{message}");
}

#[test]
fn releasing_a_savepoint_outside_a_transaction_block_is_refused() {
    // PostgreSQL: ERROR: RELEASE SAVEPOINT can only be used in transaction
    // blocks.
    let message = refusal(&format!("{TABLE} RELEASE SAVEPOINT z;"));
    assert!(message.contains("RELEASE SAVEPOINT"), "{message}");
    assert!(message.contains("transaction block"), "{message}");
}

#[test]
fn rolling_back_to_a_savepoint_outside_a_transaction_block_is_refused() {
    // PostgreSQL: ERROR: ROLLBACK TO SAVEPOINT can only be used in transaction
    // blocks.
    let message = refusal(&format!("{TABLE} ROLLBACK TO SAVEPOINT z;"));
    assert!(message.contains("SAVEPOINT"), "{message}");
    assert!(message.contains("transaction block"), "{message}");
}

#[test]
fn savepoints_inside_a_transaction_block_still_work() {
    // Measured on the server: 1 and 3 survive, 2 is rolled back to the
    // savepoint.
    assert_eq!(
        answer(&format!(
            "{TABLE} BEGIN; INSERT INTO t VALUES (1); SAVEPOINT z; \
             INSERT INTO t VALUES (2); ROLLBACK TO SAVEPOINT z; RELEASE SAVEPOINT z; \
             INSERT INTO t VALUES (3); COMMIT; SELECT a FROM t ORDER BY a;"
        )),
        vec![Some("1".to_string()), Some("3".to_string())]
    );
}

#[test]
fn a_chained_commit_with_no_transaction_open_is_refused() {
    // PostgreSQL: ERROR: COMMIT AND CHAIN can only be used in transaction
    // blocks, unlike the bare COMMIT it warns about.
    let message = refusal(&format!("{TABLE} COMMIT AND CHAIN;"));
    assert!(message.contains("AND CHAIN"), "{message}");
    assert!(message.contains("transaction block"), "{message}");
}

#[test]
fn creating_an_index_concurrently_inside_a_transaction_block_is_refused() {
    // PostgreSQL: ERROR: CREATE INDEX CONCURRENTLY cannot run inside a
    // transaction block. The replica built the index and warned only that the
    // lock differs.
    let message =
        refusal(&format!("{TABLE} BEGIN; CREATE INDEX CONCURRENTLY i ON t (a); ROLLBACK;"));
    assert!(message.contains("CONCURRENTLY"), "{message}");
    assert!(message.contains("transaction block"), "{message}");
}

#[test]
fn creating_an_index_concurrently_outside_a_transaction_block_still_works() {
    // Legal on the server, where only the lock the build takes differs.
    assert_eq!(
        answer(&format!(
            "{TABLE} INSERT INTO t VALUES (1); CREATE INDEX CONCURRENTLY i ON t (a); \
             SELECT count(*) FROM t;"
        )),
        vec![Some("1".to_string())]
    );
}

#[test]
fn setting_the_transaction_isolation_level_is_dropped_not_refused() {
    // PostgreSQL accepts it inside the open transaction and the insert
    // commits, so the count is 1. SQLite serialises writers, which is at least
    // as strict as any level PostgreSQL can name, which is exactly why the
    // BEGIN translation already drops the same clause.
    assert_eq!(
        answer(&format!(
            "{TABLE} BEGIN; SET TRANSACTION ISOLATION LEVEL SERIALIZABLE; \
             INSERT INTO t VALUES (1); COMMIT; SELECT count(*) FROM t;"
        )),
        vec![Some("1".to_string())]
    );
}

#[test]
fn setting_the_session_isolation_level_is_dropped_not_refused() {
    assert_eq!(
        answer(&format!(
            "{TABLE} SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE; \
             INSERT INTO t VALUES (1); SELECT count(*) FROM t;"
        )),
        vec![Some("1".to_string())]
    );
}

#[test]
fn a_read_only_transaction_is_refused_and_the_message_says_why() {
    // Measured: the insert inside it earns `cannot execute INSERT in a
    // read-only transaction` and the table stays empty, so dropping the clause
    // would turn that error into a write that succeeds.
    let message = refusal(&format!(
        "{TABLE} BEGIN; SET TRANSACTION READ ONLY; INSERT INTO t VALUES (1); COMMIT;"
    ));
    assert!(message.contains("read-only"), "{message}");
    assert!(message.contains("query_only"), "{message}");
    assert!(
        !message.contains("resolve"),
        "the old message described a configuration parameter deciding name resolution: {message}"
    );
}

#[test]
fn a_read_only_session_default_is_refused_and_the_message_says_why() {
    // Measured: every later write in the session earns the same error, so the
    // table stays empty.
    let message = refusal(&format!(
        "{TABLE} SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY; INSERT INTO t VALUES (1);"
    ));
    assert!(message.contains("read-only"), "{message}");
    assert!(message.contains("query_only"), "{message}");
    assert!(
        !message.contains("resolve"),
        "the old message described a configuration parameter deciding name resolution: {message}"
    );
}

#[test]
fn importing_another_transactions_snapshot_is_refused() {
    // The transaction is meant to see exactly what another one sees, and
    // SQLite has no way to join a snapshot, so dropping it would leave the
    // transaction reading its own.
    let message =
        refusal(&format!("{TABLE} BEGIN; SET TRANSACTION SNAPSHOT '00000003-0000001B-1';"));
    assert!(message.contains("SNAPSHOT"), "{message}");
    assert!(message.to_lowercase().contains("another transaction"), "{message}");
}
