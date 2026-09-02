//! Semantic correctness tests for `Pg2Sqlite::reverse_sql`.
//!
//! The reverse direction's guarantee: its output, when run on PostgreSQL,
//! produces the same final row state as the SQLite statement that was
//! translated. These tests pin that guarantee by running the original SQLite
//! statement on an in-memory database and then asserting that the
//! reverse-translated PostgreSQL SQL describes the same transition.
//!
//! The critical case is `INSERT OR REPLACE` with a partial column list.
//! SQLite's replace is DELETE-then-INSERT, so any column not named in the
//! INSERT reverts to its default (NULL here). The reverse translation must
//! emit `DO UPDATE SET col = DEFAULT` for those omitted columns, not
//! `DO NOTHING`, because `DO NOTHING` would silently preserve the old values
//! on the PostgreSQL replica.

use diesel::{RunQueryDsl, SqliteConnection, prelude::*};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;

// Typed schema for the fixture table. Columns s and n are nullable because
// INSERT OR REPLACE with a partial list leaves them as NULL.
diesel::table! {
    /// Fixture table used throughout this test module.
    t (id) {
        /// Primary key.
        id -> Integer,
        /// Text column, nullable because default is NULL.
        s -> Nullable<Text>,
        /// Integer column, nullable because default is NULL.
        n -> Nullable<Integer>,
    }
}

/// Row type for typed reads from the fixture table.
#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = t)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct TRow {
    /// Primary key.
    id: i32,
    /// Text column.
    s: Option<String>,
    /// Integer column.
    n: Option<i32>,
}

/// Schema object used by `reverse_sql`. Kept in sync with the `table!` above.
fn fixture_schema() -> ParserDB {
    Pg2Sqlite::default()
        .sql("CREATE TABLE t (id INT PRIMARY KEY, s TEXT, n INT);")
        .expect("fixture parses")
        .build_schema()
        .expect("fixture builds schema")
}

/// In-memory SQLite connection with the fixture table and one initial row
/// `(id=1, s='old', n=7)`.
fn fixture_conn() -> SqliteConnection {
    // DDL cannot be expressed via the typed DSL; use sql_query for setup only.
    let mut conn = SqliteConnection::establish(":memory:").expect("in-memory SQLite");
    diesel::sql_query("CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT, n INTEGER)")
        .execute(&mut conn)
        .expect("DDL");
    diesel::insert_into(t::table)
        .values((t::id.eq(1_i32), t::s.eq("old"), t::n.eq(7_i32)))
        .execute(&mut conn)
        .expect("initial row");
    conn
}

fn row1(conn: &mut SqliteConnection) -> TRow {
    t::table.find(1).select(TRow::as_select()).first::<TRow>(conn).expect("row 1 must exist")
}

fn reverse(sql: &str) -> String {
    Pg2Sqlite::default()
        .reverse_sql(sql, &fixture_schema(), &Pg2SqliteOptions::default())
        .expect("reverse translates")
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Full column list: every non-PK column is named in the INSERT.
///
/// SQLite's DELETE-then-INSERT and PostgreSQL's `DO UPDATE SET col =
/// EXCLUDED.col` both replace the named columns with the incoming values; the
/// final row is the same on both engines.
#[test]
fn full_column_list_sqlite_state_matches_pg_upsert() {
    let mut conn = fixture_conn();

    // `replace_into` is the typed diesel form of `REPLACE INTO` / `INSERT OR
    // REPLACE`.
    diesel::replace_into(t::table)
        .values((t::id.eq(1_i32), t::s.eq("new"), t::n.eq(42_i32)))
        .execute(&mut conn)
        .expect("replace");

    let row = row1(&mut conn);
    assert_eq!(row.id, 1, "the replaced row keeps its key: {row:?}");
    assert_eq!(row.s.as_deref(), Some("new"), "SQLite replace overwrites s: {row:?}");
    assert_eq!(row.n, Some(42), "SQLite replace overwrites n: {row:?}");

    // The reverse-translated PostgreSQL must assign EXCLUDED.<col> for every
    // named non-PK column, producing the same final values.
    let pg = reverse("INSERT OR REPLACE INTO t (id, s, n) VALUES (1, 'new', 42)");
    assert!(pg.contains("s = EXCLUDED.s"), "named column gets EXCLUDED assignment: {pg}");
    assert!(pg.contains("n = EXCLUDED.n"), "named column gets EXCLUDED assignment: {pg}");
    // No column was omitted, so DEFAULT must not appear.
    assert!(!pg.contains("DEFAULT"), "no DEFAULT when all columns are named: {pg}");
}

/// Partial column list: only the primary key is named in the INSERT.
///
/// SQLite deletes the old row (discarding s='old', n=7) and inserts a new one
/// where the omitted columns revert to NULL (no DEFAULT defined). The previous
/// reverse translation emitted `DO NOTHING`, which would have left the old row
/// intact on the PostgreSQL replica. The correct form is `DO UPDATE SET s =
/// DEFAULT, n = DEFAULT`, which resets both to NULL, matching SQLite's result.
#[test]
fn partial_column_list_resets_omitted_columns_to_default() {
    let mut conn = fixture_conn();

    // `replace_into` with only (id) is the typed diesel equivalent of
    // `INSERT OR REPLACE INTO t (id) VALUES (1)`. Diesel will omit s and n
    // from the column list, so SQLite restores them from their defaults (NULL).
    diesel::replace_into(t::table).values(t::id.eq(1_i32)).execute(&mut conn).expect("replace");

    let row = row1(&mut conn);
    // SQLite's DELETE-then-INSERT with omitted columns: the old values are
    // gone.
    assert_eq!(row.id, 1, "the replaced row keeps its key: {row:?}");
    assert_eq!(row.s, None, "omitted column s resets to NULL after SQLite replace: {row:?}");
    assert_eq!(row.n, None, "omitted column n resets to NULL after SQLite replace: {row:?}");

    // The reverse-translated PostgreSQL must emit DEFAULT for every omitted
    // non-PK column so those columns reset to NULL, matching what SQLite did.
    // DO NOTHING would be wrong: it would preserve s='old' and n=7 on the
    // PostgreSQL replica, a silent semantic inversion.
    let pg = reverse("INSERT OR REPLACE INTO t (id) VALUES (1)");
    assert!(pg.contains("DO UPDATE SET"), "must not fall back to DO NOTHING: {pg}");
    assert!(pg.contains("s = DEFAULT"), "omitted s gets DEFAULT assignment: {pg}");
    assert!(pg.contains("n = DEFAULT"), "omitted n gets DEFAULT assignment: {pg}");
    assert!(!pg.contains("EXCLUDED.s"), "omitted column must not reference EXCLUDED: {pg}");
    assert!(!pg.contains("EXCLUDED.n"), "omitted column must not reference EXCLUDED: {pg}");
}

/// `INSERT OR FAIL` and `INSERT OR ABORT` abort the statement on conflict,
/// which is exactly what a plain PostgreSQL `INSERT` does. The OR clause is
/// dropped and no `ON CONFLICT` is emitted.
#[test]
fn insert_or_fail_and_abort_translate_to_plain_insert() {
    let schema = fixture_schema();
    let opts = Pg2SqliteOptions::default();

    // INSERT OR FAIL and INSERT OR ABORT have no typed diesel equivalent; the
    // SQLite-specific conflict clause cannot be expressed via the DSL.
    for sql in &[
        "INSERT OR FAIL INTO t (id, s) VALUES (2, 'x')",
        "INSERT OR ABORT INTO t (id, s) VALUES (2, 'x')",
    ] {
        let pg = Pg2Sqlite::default()
            .reverse_sql(sql, &schema, &opts)
            .unwrap_or_else(|e| panic!("{sql} should translate, got: {e}"))
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        assert!(!pg.contains("OR FAIL"), "OR FAIL must be dropped: {pg}");
        assert!(!pg.contains("OR ABORT"), "OR ABORT must be dropped: {pg}");
        assert!(!pg.contains("ON CONFLICT"), "no conflict clause emitted: {pg}");
    }
}
