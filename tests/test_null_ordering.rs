//! Where NULLs sort in an `ORDER BY`.
//!
//! The two databases default oppositely: PostgreSQL is ASC NULLS LAST and DESC
//! NULLS FIRST, SQLite is ASC NULLS FIRST and DESC NULLS LAST. Stripping the
//! clause therefore does not preserve the order, it inverts it.
//!
//! Measured on PostgreSQL 16 over `(1,'b',NULL)`, `(2,NULL,'x')`,
//! `(3,'a','y')`:
//!
//! | ordering | ids |
//! |---|---|
//! | `ORDER BY s` | 3, 1, 2 |
//! | `ORDER BY s DESC` | 2, 1, 3 |
//! | `ORDER BY s ASC NULLS FIRST` | 2, 3, 1 |
//! | `ORDER BY s DESC NULLS LAST` | 1, 3, 2 |
//! | `ORDER BY s LIMIT 1` | 3 |
//! | `ORDER BY t, s` | 2, 3, 1 |

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::Pg2SqliteOptions;
use run_translated_helper::run_translated_with;

const TABLE: &str = "CREATE TABLE t (id INT PRIMARY KEY, s TEXT, u TEXT);
     INSERT INTO t VALUES (1, 'b', NULL), (2, NULL, 'x'), (3, 'a', 'y');";

/// The ids in the order the emitted query returns them.
fn order(ordering: &str) -> Option<String> {
    run_translated_with(
        &format!("{TABLE} SELECT group_concat(id) FROM (SELECT id FROM t ORDER BY {ordering});"),
        &Pg2SqliteOptions::default(),
    )
    .into_iter()
    .next()
    .expect("one row")
}

/// Neither spelling carries a marker distinguishing "the author wrote ASC" from
/// "the author wrote nothing", and both need NULLS LAST.
#[test]
fn the_implicit_ascending_default_puts_nulls_last() {
    assert_eq!(order("s"), Some("3,1,2".to_string()));
    assert_eq!(order("s ASC"), Some("3,1,2".to_string()));
}

#[test]
fn the_implicit_descending_default_puts_nulls_first() {
    assert_eq!(order("s DESC"), Some("2,1,3".to_string()));
}

/// Both spellings invert the default this change introduces, so they prove the
/// explicit clause survives rather than being shadowed by the default.
#[test]
fn an_explicit_clause_overrides_the_default() {
    assert_eq!(order("s ASC NULLS FIRST"), Some("2,3,1".to_string()));
    assert_eq!(order("s DESC NULLS LAST"), Some("1,3,2".to_string()));
}

/// With a LIMIT the wrong row comes back rather than the right rows in the
/// wrong order, which is the case that turns a display bug into a data bug.
#[test]
fn a_limited_query_returns_the_postgres_row() {
    let rows = run_translated_with(
        &format!("{TABLE} SELECT id FROM t ORDER BY s LIMIT 1;"),
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("3".to_string())]);
}

/// Every key needs its own default, not just the first.
#[test]
fn each_key_of_a_multi_key_ordering_gets_its_default() {
    assert_eq!(order("u, s"), Some("2,3,1".to_string()));
}
