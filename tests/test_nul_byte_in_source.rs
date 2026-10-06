//! PostgreSQL's wire protocol sends query text as a NUL-terminated string,
//! so a NUL byte inside it can never reach the server intact; `sqlparser`
//! has no such limit, so a round trip through `Pg2Sqlite::sql` emitted a
//! NUL into the SQLite text that then truncated mid-statement at execution.

use pg2sqlite::prelude::Pg2Sqlite;

#[test]
fn a_nul_byte_inside_a_string_literal_is_refused() {
    let sql = "SELECT 2, '[0.4, 0.\x005, 0.6]';";
    let result = Pg2Sqlite::default().sql(sql);
    assert!(result.is_err(), "expected a NUL byte in the source to be refused");
}

#[test]
fn a_nul_byte_anywhere_in_the_source_is_refused() {
    let sql = "CREATE TABLE t (a INT);\n-- comment with a \0 byte\n";
    let result = Pg2Sqlite::default().sql(sql);
    assert!(result.is_err(), "expected a NUL byte anywhere in the source to be refused");
}

#[test]
fn source_without_a_nul_byte_still_parses() {
    let result = Pg2Sqlite::default().sql("SELECT 2, '[0.4, 0.5, 0.6]';");
    assert!(result.is_ok());
}
