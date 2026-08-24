//! Parity for text, collation, null, and boolean constructs.
//!
//! One table seeded with ASCII rows covers most constructs. Each test
//! runs an expression on both engines and either asserts agreement or
//! records a finding when a known divergence appears.

use diesel::{pg::PgConnection, prelude::*, sqlite::SqliteConnection};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use postgres_harness::Outcome;

use crate::{helpers, postgres_harness};

// DDL in PostgreSQL dialect; the SQLite side translates it.
const TABLE_DDL: &str =
    "CREATE TABLE text_parity (id INTEGER PRIMARY KEY, s TEXT NOT NULL, n TEXT);";

mod schema {
    diesel::table! {
        text_parity (id) {
            id -> Integer,
            s -> Text,
            n -> Nullable<Text>,
        }
    }
}

use schema::text_parity;

#[derive(Insertable)]
#[diesel(table_name = text_parity)]
struct Row {
    id: i32,
    s: String,
    n: Option<String>,
}

fn seed_rows() -> Vec<Row> {
    vec![
        Row { id: 1, s: "Hello".into(), n: Some("world".into()) },
        Row { id: 2, s: String::new(), n: None },
        Row { id: 3, s: "ABC".into(), n: Some("abc".into()) },
        Row { id: 4, s: "a%b_c".into(), n: Some("A%B_C".into()) },
        Row { id: 5, s: "hello".into(), n: Some("HELLO".into()) },
        Row { id: 6, s: "  hi  ".into(), n: Some("  bye  ".into()) },
    ]
}

fn pg_conn() -> PgConnection {
    let mut conn = postgres_harness::fresh_database();
    postgres_harness::apply(&mut conn, TABLE_DDL).expect("create table on pg");
    diesel::insert_into(text_parity::table)
        .values(seed_rows())
        .execute(&mut conn)
        .expect("seed rows on pg");
    conn
}

fn sqlite_conn() -> SqliteConnection {
    // The translator output is a runtime string; raw sql_query is the correct
    // form for applying translated DDL.
    let translated = Pg2Sqlite::default()
        .sql(TABLE_DDL)
        .expect("parse table DDL")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate table DDL");
    let mut conn = establish_connection();
    for stmt in &translated {
        diesel::sql_query(stmt.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
    }
    diesel::insert_into(text_parity::table)
        .values(seed_rows())
        .execute(&mut conn)
        .expect("seed rows on sqlite");
    conn
}

// The SELECT expression queries below use diesel::sql_query because:
// (1) The SQLite side runs Pg2Sqlite translator output, a runtime string.
// (2) PG-specific syntax (ILIKE, char_length, POSITION IN) has no
//     cross-backend typed Diesel DSL equivalent.
// (3) Symmetric comparison requires the same textual SQL on the PG side.
#[derive(QueryableByName)]
struct Val {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    val: Option<String>,
}

fn pg_rows(conn: &mut PgConnection, sql: &str) -> Outcome {
    match diesel::sql_query(sql).load::<Val>(conn) {
        Ok(rows) => {
            Outcome::Rows(
                rows.into_iter().map(|r| r.val.unwrap_or_else(|| "NULL".to_string())).collect(),
            )
        }
        Err(_) => Outcome::Refused,
    }
}

fn sqlite_rows(conn: &mut SqliteConnection, sql: &str) -> Outcome {
    match diesel::sql_query(sql).load::<Val>(conn) {
        Ok(rows) => {
            Outcome::Rows(
                rows.into_iter().map(|r| r.val.unwrap_or_else(|| "NULL".to_string())).collect(),
            )
        }
        Err(_) => Outcome::Refused,
    }
}

/// Translates a PG SELECT with the test table as schema context.
///
/// TABLE_DDL is prepended so the translator can resolve column references.
/// The last translated statement is the SELECT.
fn translate_select(pg_select: &str) -> String {
    let stmts = Pg2Sqlite::default()
        .sql(TABLE_DDL)
        .expect("parse DDL context")
        .sql(pg_select)
        .expect("parse PG SELECT")
        .translate(&Pg2SqliteOptions::default())
        .expect("translate SELECT");
    stmts.last().expect("non-empty translation output").to_string()
}

/// Prints a finding when the two engines diverge on a construct.
fn finding(label: &str, pg: &Outcome, sqlite: &Outcome) {
    println!("FINDING {label}: pg={pg:?} sqlite={sqlite:?}");
}

/// lower() and upper() fold ASCII identically on both engines.
#[test]
fn case_folding_agrees_on_ascii() {
    let mut pg = pg_conn();
    let mut sqlite = sqlite_conn();

    let sql = "SELECT lower(s) AS val FROM text_parity ORDER BY id";
    assert_eq!(
        sqlite_rows(&mut sqlite, sql),
        pg_rows(&mut pg, sql),
        "lower() on ASCII must agree between engines"
    );

    let sql = "SELECT upper(s) AS val FROM text_parity ORDER BY id";
    assert_eq!(
        sqlite_rows(&mut sqlite, sql),
        pg_rows(&mut pg, sql),
        "upper() on ASCII must agree between engines"
    );
}

/// COALESCE, NULLIF, and bare || all handle NULL identically on both engines.
#[test]
fn null_semantics_agree() {
    let mut pg = pg_conn();
    let mut sqlite = sqlite_conn();

    let sql = "SELECT COALESCE(n, 'fallback') AS val FROM text_parity ORDER BY id";
    assert_eq!(
        sqlite_rows(&mut sqlite, sql),
        pg_rows(&mut pg, sql),
        "COALESCE must agree between engines"
    );

    let sql = "SELECT NULLIF(s, '') AS val FROM text_parity ORDER BY id";
    assert_eq!(
        sqlite_rows(&mut sqlite, sql),
        pg_rows(&mut pg, sql),
        "NULLIF must agree between engines"
    );

    // Both engines propagate NULL through bare ||.
    let sql = "SELECT s || n AS val FROM text_parity ORDER BY id";
    assert_eq!(
        sqlite_rows(&mut sqlite, sql),
        pg_rows(&mut pg, sql),
        "bare || must propagate NULL on both engines"
    );
}

/// TRIM, LTRIM, and RTRIM strip spaces identically on both engines.
#[test]
fn trim_functions_agree() {
    let mut pg = pg_conn();
    let mut sqlite = sqlite_conn();

    let sql = "SELECT TRIM(s) AS val FROM text_parity ORDER BY id";
    assert_eq!(
        sqlite_rows(&mut sqlite, sql),
        pg_rows(&mut pg, sql),
        "TRIM must agree between engines"
    );

    let sql = "SELECT LTRIM(s) AS val FROM text_parity ORDER BY id";
    assert_eq!(
        sqlite_rows(&mut sqlite, sql),
        pg_rows(&mut pg, sql),
        "LTRIM must agree between engines"
    );

    let sql = "SELECT RTRIM(s) AS val FROM text_parity ORDER BY id";
    assert_eq!(
        sqlite_rows(&mut sqlite, sql),
        pg_rows(&mut pg, sql),
        "RTRIM must agree between engines"
    );
}

/// LIKE case sensitivity diverges between engines; LIKE ESCAPE and translated
/// ILIKE must agree.
#[test]
fn like_behaviour_compared() {
    let mut pg = pg_conn();
    let mut sqlite = sqlite_conn();

    // PG LIKE is case-sensitive; SQLite LIKE is case-insensitive for ASCII.
    // This is a known engine divergence, not a translator defect.
    let sql = "SELECT CASE WHEN s LIKE 'hello' THEN 'match' ELSE 'no' END AS val \
               FROM text_parity ORDER BY id";
    let pg_out = pg_rows(&mut pg, sql);
    let sqlite_out = sqlite_rows(&mut sqlite, sql);
    if pg_out != sqlite_out {
        finding(
            "LIKE case sensitivity: PG is case-sensitive, SQLite is case-insensitive for ASCII",
            &pg_out,
            &sqlite_out,
        );
    }

    // LIKE ESCAPE must work the same on both engines.
    // The pattern 'a!%b!_c' ESCAPE '!' matches the literal string 'a%b_c'.
    let sql = "SELECT CASE WHEN s LIKE 'a!%b!_c' ESCAPE '!' THEN 'match' ELSE 'no' END AS val \
               FROM text_parity ORDER BY id";
    assert_eq!(
        sqlite_rows(&mut sqlite, sql),
        pg_rows(&mut pg, sql),
        "LIKE ESCAPE must agree between engines"
    );

    // ILIKE (PG-only) translates to a case-insensitive form for SQLite.
    // For ASCII rows the translated form must agree with the PG original.
    let pg_sql = "SELECT CASE WHEN s ILIKE 'hello' THEN 'match' ELSE 'no' END AS val \
                  FROM text_parity ORDER BY id";
    let sqlite_sql = translate_select(pg_sql);
    assert_eq!(
        sqlite_rows(&mut sqlite, &sqlite_sql),
        pg_rows(&mut pg, pg_sql),
        "ILIKE must agree after translation"
    );
}

/// PG-specific string functions produce correct results after translation.
#[test]
fn pg_string_functions_translate_correctly() {
    let mut pg = pg_conn();
    let mut sqlite = sqlite_conn();

    // char_length (PG) translates to length in SQLite.
    {
        let pg_sql = "SELECT CAST(char_length(s) AS TEXT) AS val FROM text_parity ORDER BY id";
        let sqlite_sql = translate_select(pg_sql);
        assert_eq!(
            sqlite_rows(&mut sqlite, &sqlite_sql),
            pg_rows(&mut pg, pg_sql),
            "char_length must agree after translation"
        );
    }

    // SUBSTRING FROM FOR (PG syntax) translates to SUBSTR.
    {
        let pg_sql = "SELECT SUBSTRING(s FROM 2 FOR 3) AS val FROM text_parity WHERE id = 1";
        let sqlite_sql = translate_select(pg_sql);
        assert_eq!(
            sqlite_rows(&mut sqlite, &sqlite_sql),
            pg_rows(&mut pg, pg_sql),
            "SUBSTRING FROM FOR must agree after translation"
        );
    }

    // POSITION IN (PG syntax) translates to INSTR.
    {
        let pg_sql = "SELECT CAST(POSITION('l' IN s) AS TEXT) AS val FROM text_parity ORDER BY id";
        let sqlite_sql = translate_select(pg_sql);
        assert_eq!(
            sqlite_rows(&mut sqlite, &sqlite_sql),
            pg_rows(&mut pg, pg_sql),
            "POSITION IN must agree after translation"
        );
    }

    // CONCAT ignores NULL in PG; the translation must preserve that behaviour.
    {
        let pg_sql = "SELECT CONCAT(s, n) AS val FROM text_parity ORDER BY id";
        let sqlite_sql = translate_select(pg_sql);
        assert_eq!(
            sqlite_rows(&mut sqlite, &sqlite_sql),
            pg_rows(&mut pg, pg_sql),
            "CONCAT must agree after translation"
        );
    }
}

/// CAST(TRUE AS TEXT) returns 'true' in PG and '1' in SQLite without
/// translation; the translator must emit the correct form.
#[test]
fn boolean_cast_to_text_translates_correctly() {
    let mut pg = pg_conn();
    let mut sqlite = sqlite_conn();

    let pg_sql = "SELECT CAST(TRUE AS TEXT) AS val";
    let sqlite_sql = translate_select(pg_sql);
    assert_eq!(
        sqlite_rows(&mut sqlite, &sqlite_sql),
        pg_rows(&mut pg, pg_sql),
        "CAST(TRUE AS TEXT) must agree after translation"
    );
}

/// `ascii('')` answers 0 on PostgreSQL, and the rename target `unicode('')`
/// answers NULL on SQLite, so a plain rename is not enough.
#[test]
fn ascii_of_the_empty_string_agrees() {
    let mut pg = pg_conn();
    let mut sqlite = sqlite_conn();

    let pg_sql = "SELECT CAST(ascii('') AS TEXT) AS val FROM text_parity WHERE id = 1";
    let sqlite_sql = translate_select(pg_sql);
    assert_eq!(
        sqlite_rows(&mut sqlite, &sqlite_sql),
        pg_rows(&mut pg, pg_sql),
        "ascii('') must agree after translation"
    );
}
