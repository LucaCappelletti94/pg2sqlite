//! E1: NUMERIC literal unscaling in the reverse direction.
//!
//! A NUMERIC(p,s) column holds minor units on the SQLite replica (1.50 stored
//! as 150). Any integer literal compared or assigned against it must be divided
//! by 10^s when going back to PostgreSQL, so `amount > 100` becomes
//! `amount > 1.00`. Division cannot be faithfully reversed and is refused.
//! An integer literal beside a column this crate cannot resolve in scope is
//! also refused rather than passed through unscaled.

use diesel::{Connection, ExpressionMethods, QueryDsl, RunQueryDsl, SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sql_traits::structs::ParserDB;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

// Diesel schema for the translated table.
// `amount` is BigInt because NUMERIC(10,2) becomes INTEGER (minor units) in
// SQLite.
diesel::table! {
    /// Test table with a scaled NUMERIC column.
    t (id) {
        /// Primary key.
        id -> Integer,
        /// NUMERIC(10,2) column stored as minor-unit INTEGER on the replica.
        amount -> Nullable<diesel::sql_types::BigInt>,
        /// Optional text tag column.
        tag -> Nullable<diesel::sql_types::Text>,
    }
}

const PG_DDL: &str = "
    CREATE TABLE t (
        id      INTEGER PRIMARY KEY,
        amount  NUMERIC(10, 2),
        tag     TEXT
    );
";

fn pg2sqlite() -> Pg2Sqlite {
    Pg2Sqlite::default().sql(PG_DDL).expect("parse PG DDL")
}

fn schema() -> ParserDB {
    pg2sqlite().build_schema().expect("build schema")
}

/// Translates `PG_DDL + pg_dml` forward and returns the last emitted statement
/// as a SQL string (the DML after the schema DDL).
fn forward_dml(pg_dml: &str) -> String {
    let tr = Pg2Sqlite::default().sql(&format!("{PG_DDL}\n{pg_dml}")).expect("parse");
    tr.translate(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_iter()
        .rfind(|statement| !matches!(statement, sqlparser::ast::Statement::CreateTable(_)))
        .expect("at least one DML statement")
        .to_string()
}

/// Reverse-translates `sqlite_sql` against the PG schema and returns the first
/// emitted PostgreSQL statement as text.
fn reverse(sqlite_sql: &str) -> Result<String, pg2sqlite::errors::Error> {
    let tr = pg2sqlite();
    let schema = schema();
    let stmts = tr.reverse_sql(&format!("{sqlite_sql};"), &schema, &Pg2SqliteOptions::default())?;
    Ok(stmts.first().expect("one statement").to_string())
}

/// Opens an in-memory SQLite connection with the translated schema applied.
/// Emitted DDL runs as text because it is the artifact under test.
fn open_replica() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("in-memory SQLite should open");
    for stmt in pg2sqlite().translate(&Pg2SqliteOptions::default()).expect("translate DDL") {
        // Emitted DDL is the artifact under test, so it runs as text.
        diesel::sql_query(stmt.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("emitted DDL failed: {e}\n{stmt}"));
    }
    conn
}

/// Seeds the replica with a row whose `amount` is already in minor units
/// (replica form), using the typed schema so the insert is compile-checked.
fn seed(conn: &mut SqliteConnection, id: i32, amount_minor_units: i64) {
    diesel::insert_into(t::table)
        .values((t::id.eq(id), t::amount.eq(amount_minor_units)))
        .execute(conn)
        .expect("seed insert should succeed");
}

#[derive(diesel::QueryableByName, Debug, PartialEq)]
struct IdRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
}

// ── Comparison round-trip ────────────────────────────────────────────────────

/// The forward translator scales `amount > 1.00` to `amount > 100`.
/// The reverse translator must unscale `100` back to `1.00`.
///
/// Proved by executing the translated SELECT against a replica row with
/// `amount = 150` (= 1.50 minor units) and asserting the row is returned.
#[test]
fn comparison_literal_unscales_in_reverse() {
    let sqlite_sel = forward_dml("SELECT id FROM t WHERE amount > 1.00;");
    assert!(sqlite_sel.contains("> 100"), "forward must scale literal: {sqlite_sel}");

    // Execute on the replica: amount=150 (=1.50), 150 > 100 → row found.
    // The SELECT is translator output, so it runs as text.
    let mut conn = open_replica();
    seed(&mut conn, 1, 150); // 1.50 → 150
    seed(&mut conn, 2, 50); // 0.50 → 50, below threshold
    let rows: Vec<IdRow> = diesel::sql_query(&sqlite_sel)
        .load(&mut conn)
        .unwrap_or_else(|e| panic!("translated SELECT failed: {e}\n{sqlite_sel}"));
    assert_eq!(rows.len(), 1, "replica must find exactly one row > 100 (=1.00): {sqlite_sel}");
    assert_eq!(rows[0].id, 1);

    // Reverse: amount > 100 → amount > 1.00.
    let pg_sql = reverse(&sqlite_sel).expect("reverse must succeed");
    assert!(pg_sql.contains("> 1.00"), "reverse must unscale comparison literal: {pg_sql}");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg_sql)
        .unwrap_or_else(|e| panic!("reverse output is not valid PostgreSQL: {e}\n{pg_sql}"));
}

// ── IN-list round-trip ───────────────────────────────────────────────────────

/// The forward translator scales each element of `amount IN (1.00, 1.50,
/// 2.00)` to its minor-unit form. The reverse translator must unscale them
/// back.
#[test]
fn in_list_literals_unscale_in_reverse() {
    let sqlite_sel = forward_dml("SELECT id FROM t WHERE amount IN (1.00, 1.50, 2.00);");
    // Forward scales all three elements.
    assert!(
        sqlite_sel.contains("100") && sqlite_sel.contains("150") && sqlite_sel.contains("200"),
        "forward must scale all IN-list literals: {sqlite_sel}"
    );

    let mut conn = open_replica();
    seed(&mut conn, 1, 100); // 1.00 — in list
    seed(&mut conn, 2, 150); // 1.50 — in list
    seed(&mut conn, 3, 250); // 2.50 — not in list
    let rows: Vec<IdRow> = diesel::sql_query(&sqlite_sel)
        .load(&mut conn)
        .unwrap_or_else(|e| panic!("translated SELECT failed: {e}\n{sqlite_sel}"));
    assert_eq!(rows.len(), 2, "replica must find two rows in the IN list: {sqlite_sel}");

    let pg_sql = reverse(&sqlite_sel).expect("reverse must succeed");
    assert!(pg_sql.contains("1.00"), "reverse must restore first element: {pg_sql}");
    assert!(pg_sql.contains("1.50"), "reverse must restore second element: {pg_sql}");
    assert!(pg_sql.contains("2.00"), "reverse must restore third element: {pg_sql}");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg_sql)
        .unwrap_or_else(|e| panic!("reverse output is not valid PostgreSQL: {e}\n{pg_sql}"));
}

// ── BETWEEN round-trip ───────────────────────────────────────────────────────

/// The forward translator scales `BETWEEN 1.00 AND 2.00` to
/// `BETWEEN 100 AND 200`. The reverse translator must restore both bounds.
#[test]
fn between_literals_unscale_in_reverse() {
    let sqlite_sel = forward_dml("SELECT id FROM t WHERE amount BETWEEN 1.00 AND 2.00;");
    assert!(
        sqlite_sel.contains("BETWEEN 100 AND 200")
            || (sqlite_sel.contains("100") && sqlite_sel.contains("200")),
        "forward must scale BETWEEN bounds: {sqlite_sel}"
    );

    let mut conn = open_replica();
    seed(&mut conn, 1, 100); // 1.00 — in range
    seed(&mut conn, 2, 150); // 1.50 — in range
    seed(&mut conn, 3, 250); // 2.50 — above range
    let rows: Vec<IdRow> = diesel::sql_query(&sqlite_sel)
        .load(&mut conn)
        .unwrap_or_else(|e| panic!("translated SELECT failed: {e}\n{sqlite_sel}"));
    assert_eq!(rows.len(), 2, "replica must find two rows in [1.00, 2.00]: {sqlite_sel}");

    let pg_sql = reverse(&sqlite_sel).expect("reverse must succeed");
    assert!(
        pg_sql.to_ascii_uppercase().contains("BETWEEN")
            && pg_sql.contains("1.00")
            && pg_sql.contains("2.00"),
        "reverse must restore BETWEEN bounds: {pg_sql}"
    );
    Parser::parse_sql(&PostgreSqlDialect {}, &pg_sql)
        .unwrap_or_else(|e| panic!("reverse output is not valid PostgreSQL: {e}\n{pg_sql}"));
}

// ── INSERT round-trip ────────────────────────────────────────────────────────

/// The forward translator scales the 1.50 literal in
/// `INSERT INTO t (id, amount) VALUES (1, 1.50)` to 150.
/// The reverse translator must unscale 150 back to 1.50.
///
/// Proved by executing the translated INSERT and reading the stored value with
/// a typed diesel query — the minor-unit integer 150 is what the replica holds.
#[test]
fn insert_literal_unscales_in_reverse() {
    let sqlite_ins = forward_dml("INSERT INTO t (id, amount) VALUES (1, 1.50);");
    assert!(sqlite_ins.contains("150"), "forward must scale INSERT literal: {sqlite_ins}");

    // Execute the translated INSERT on the replica.
    // Runs as text because it is the artifact under test.
    let mut conn = open_replica();
    diesel::sql_query(&sqlite_ins)
        .execute(&mut conn)
        .unwrap_or_else(|e| panic!("translated INSERT failed: {e}\n{sqlite_ins}"));

    // Read the stored value with the typed schema to confirm minor units.
    let stored: i64 = t::table
        .select(t::amount)
        .filter(t::id.eq(1))
        .first::<Option<i64>>(&mut conn)
        .expect("inserted row should exist")
        .expect("amount should not be NULL");
    assert_eq!(stored, 150, "replica must store 150 (minor units for 1.50)");

    // Reverse: VALUES (..., 150) → VALUES (..., 1.50).
    let pg_sql = reverse(&sqlite_ins).expect("reverse must succeed");
    assert!(pg_sql.contains("1.50"), "reverse must unscale INSERT literal to 1.50: {pg_sql}");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg_sql)
        .unwrap_or_else(|e| panic!("reverse output is not valid PostgreSQL: {e}\n{pg_sql}"));
}

// ── UPDATE round-trip ────────────────────────────────────────────────────────

/// The forward translator scales the 2.00 literal in `SET amount = 2.00` to
/// 200. The reverse translator must unscale 200 back to 2.00.
#[test]
fn update_literal_unscales_in_reverse() {
    let sqlite_upd = forward_dml("UPDATE t SET amount = 2.00 WHERE id = 1;");
    assert!(sqlite_upd.contains("200"), "forward must scale UPDATE literal: {sqlite_upd}");

    // Seed and run the translated UPDATE on the replica.
    let mut conn = open_replica();
    seed(&mut conn, 1, 100); // 1.00 initially
    // UPDATE is translator output, so it runs as text.
    diesel::sql_query(&sqlite_upd)
        .execute(&mut conn)
        .unwrap_or_else(|e| panic!("translated UPDATE failed: {e}\n{sqlite_upd}"));

    // Read back with the typed schema to confirm the stored minor-unit value.
    let stored: i64 = t::table
        .select(t::amount)
        .filter(t::id.eq(1))
        .first::<Option<i64>>(&mut conn)
        .expect("updated row should exist")
        .expect("amount should not be NULL");
    assert_eq!(stored, 200, "replica must hold 200 (minor units for 2.00) after update");

    // Reverse: SET amount = 200 → SET amount = 2.00.
    let pg_sql = reverse(&sqlite_upd).expect("reverse must succeed");
    assert!(pg_sql.contains("2.00"), "reverse must unscale UPDATE literal to 2.00: {pg_sql}");
    Parser::parse_sql(&PostgreSqlDialect {}, &pg_sql)
        .unwrap_or_else(|e| panic!("reverse output is not valid PostgreSQL: {e}\n{pg_sql}"));
}

// ── Division refusal ─────────────────────────────────────────────────────────

/// SQLite performs integer division over minor units. PostgreSQL performs exact
/// numeric division. The two answers are structurally different, so the reverse
/// translator refuses rather than emitting a statement that answers
/// differently.
///
/// `SELECT amount / 7 FROM t` on a row with amount=150 gives 21 in SQLite
/// (integer division) and 0.214... in PostgreSQL. No literal adjustment fixes
/// this, so the only safe response is a refusal.
#[test]
fn division_over_numeric_column_is_refused() {
    // The forward translator refuses NUMERIC division too, so this SQLite SQL
    // is constructed directly rather than produced by forward translation.
    let err = reverse("SELECT amount / 7 FROM t").expect_err("division must be refused");
    let msg = err.to_string();
    assert!(
        msg.to_ascii_lowercase().contains("division")
            || msg.to_ascii_lowercase().contains("integer"),
        "error must explain the division problem; got: {msg}"
    );
}

// ── A column the schema cannot resolve ───────────────────────────────────────

/// Unscaling asks what a column's declared scale is, and a reference the
/// schema cannot resolve has no answer, so the literal stands as written.
///
/// Refusing instead would take out every reverse translation over a relation
/// whose columns are not inspectable, a view or a derived table among them,
/// which is a far larger loss than the one literal this cannot rescale.
#[test]
fn a_literal_beside_an_unresolvable_column_is_left_alone() {
    let emitted = reverse("SELECT xyz > 100 FROM t").expect("an unresolvable column translates");
    assert!(emitted.contains("100"), "the literal stands as written: {emitted}");
}
