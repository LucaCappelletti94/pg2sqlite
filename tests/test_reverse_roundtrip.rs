//! Tests for reverse round-trip correctness: forward-translated patterns must
//! reverse back to valid PostgreSQL.
//!
//! Two layers of testing:
//! 1. **String assertion tests** — verify expected function names appear in
//!    output
//! 2. **Functional diesel tests** — forward translate, execute in SQLite,
//!    reverse, and parse the result with the PostgreSQL dialect to prove
//!    syntactic validity

mod helpers;

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, val TEXT, num INT);";
const EVENTS: &str =
    "CREATE TABLE events (id INT PRIMARY KEY, created_at TIMESTAMP, category TEXT);";

fn reverse(pg_ddl: &str, sqlite_sql: &str) -> String {
    let translator = Pg2Sqlite::default().sql(pg_ddl).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default();
    let stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();
    assert!(!stmts.is_empty());
    stmts[0].to_string()
}

/// Forward-translate PG SQL to SQLite, execute the SELECT in an in-memory
/// SQLite database, then reverse-translate back to PG and parse the result
/// with the PostgreSQL dialect to verify it is syntactically valid.
fn forward_execute_reverse(pg_ddl: &str, pg_query: &str, options: &Pg2SqliteOptions) -> String {
    let translator = Pg2Sqlite::default().sql(&format!("{pg_ddl}\n{pg_query}")).unwrap();
    let schema = translator.build_schema().unwrap();
    let forward_stmts = translator.clone().translate(options).unwrap();

    // Set up in-memory SQLite and execute DDL
    let mut conn =
        SqliteConnection::establish(":memory:").expect("Failed to connect to in-memory SQLite");

    for stmt in &forward_stmts {
        if !matches!(stmt, sqlparser::ast::Statement::Query(_)) {
            diesel::sql_query(&stmt.to_string())
                .execute(&mut conn)
                .unwrap_or_else(|e| panic!("Failed to execute DDL: {e}\nSQL: {stmt}"));
        }
    }

    // Find the translated SELECT and execute it in SQLite
    let select_stmt = forward_stmts
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .expect("Expected a SELECT statement in translated output");
    let sqlite_sql = select_stmt.to_string();

    // Execute in SQLite to verify it's valid SQLite
    #[derive(diesel::QueryableByName, Debug)]
    #[allow(dead_code)]
    struct DynResult {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        result: Option<String>,
    }

    // Wrap in a column alias so diesel can bind by name
    let exec_sql = format!("SELECT ({sqlite_sql}) AS result");
    // Allow execution to succeed (empty table is fine -- we just need valid SQL)
    let _results = diesel::sql_query(&exec_sql)
        .load::<DynResult>(&mut conn)
        .unwrap_or_else(|e| panic!("SQLite execution failed: {e}\nSQL: {exec_sql}"));

    // Reverse-translate the SQLite SQL back to PostgreSQL
    let pg_stmts = translator.reverse_sql(&format!("{sqlite_sql};"), &schema, options).unwrap();
    assert!(!pg_stmts.is_empty(), "Reverse translation produced no statements");
    let pg_sql = pg_stmts[0].to_string();

    // Parse with PostgreSQL dialect to verify syntactic validity
    Parser::parse_sql(&PostgreSqlDialect {}, &pg_sql)
        .unwrap_or_else(|e| panic!("Reverse output is not valid PostgreSQL: {e}\nSQL: {pg_sql}"));

    pg_sql
}

#[test]
fn reverse_datetime_unixepoch_to_to_timestamp() {
    let pg = reverse(SCHEMA, "SELECT datetime(num, 'unixepoch') FROM t;");
    assert!(pg.contains("to_timestamp"), "Expected to_timestamp: {pg}");
    assert!(!pg.contains("datetime"), "Should not contain datetime: {pg}");
}

#[test]
fn reverse_uuid_to_gen_random_uuid() {
    let pg = reverse(SCHEMA, "SELECT uuid() FROM t;");
    assert!(pg.contains("gen_random_uuid"), "Expected gen_random_uuid: {pg}");
    assert!(!pg.contains(" uuid("), "Should not contain bare uuid(): {pg}");
}

#[test]
fn reverse_custom_uuid_function_name() {
    let translator = Pg2Sqlite::default().sql(SCHEMA).unwrap();
    let schema = translator.build_schema().unwrap();
    let options = Pg2SqliteOptions::default().with_uuid_function_name("uuidv7");
    let stmts = translator.reverse_sql("SELECT uuidv7() FROM t;", &schema, &options).unwrap();
    let pg = stmts[0].to_string();
    assert!(pg.contains("gen_random_uuid"), "Expected gen_random_uuid: {pg}");
}

#[test]
fn reverse_strftime_year_to_date_trunc() {
    let pg = reverse(EVENTS, "SELECT strftime('%Y-01-01 00:00:00', created_at) FROM events;");
    assert!(pg.contains("date_trunc"), "Expected date_trunc: {pg}");
    assert!(pg.contains("year"), "Expected 'year' field: {pg}");
}

#[test]
fn reverse_strftime_month_to_date_trunc() {
    let pg = reverse(EVENTS, "SELECT strftime('%Y-%m-01 00:00:00', created_at) FROM events;");
    assert!(pg.contains("date_trunc"), "Expected date_trunc: {pg}");
    assert!(pg.contains("month"), "Expected 'month' field: {pg}");
}

#[test]
fn reverse_strftime_day_to_date_trunc() {
    let pg = reverse(EVENTS, "SELECT strftime('%Y-%m-%d 00:00:00', created_at) FROM events;");
    assert!(pg.contains("date_trunc"), "Expected date_trunc: {pg}");
    assert!(pg.contains("day"), "Expected 'day' field: {pg}");
}

/// A date-only format is not a `date_trunc`: PostgreSQL's answers a timestamp,
/// so reversing it that way would change what the query returns.
#[test]
fn reverse_strftime_date_only_is_not_date_trunc() {
    let pg = reverse(EVENTS, "SELECT strftime('%Y-%m-%d', created_at) FROM events;");
    assert!(!pg.contains("date_trunc"), "a bare date is not a truncated timestamp: {pg}");
}

#[test]
fn reverse_strftime_extract_still_works() {
    // Single-token formats like %Y must still reverse to EXTRACT, not date_trunc
    let pg = reverse(EVENTS, "SELECT strftime('%Y', created_at) FROM events;");
    assert!(pg.contains("EXTRACT(YEAR"), "Expected EXTRACT(YEAR): {pg}");
    assert!(!pg.contains("date_trunc"), "Should not contain date_trunc: {pg}");
}

#[test]
fn functional_to_timestamp_roundtrip() {
    let ddl = "CREATE TABLE t (id INT PRIMARY KEY, epoch_val INT);";
    let query = "SELECT to_timestamp(epoch_val) FROM t;";
    let pg = forward_execute_reverse(ddl, query, &Pg2SqliteOptions::default());
    assert!(pg.contains("to_timestamp"), "Expected to_timestamp in round-trip: {pg}");
    assert!(!pg.contains("datetime"), "Should not contain datetime after round-trip: {pg}");
}

#[test]
fn functional_date_trunc_day_roundtrip() {
    let ddl = "CREATE TABLE events (id INT PRIMARY KEY, created_at TEXT);";
    let query = "SELECT date_trunc('day', created_at) FROM events;";
    let pg = forward_execute_reverse(ddl, query, &Pg2SqliteOptions::default());
    assert!(pg.contains("date_trunc"), "Expected date_trunc in round-trip: {pg}");
    assert!(pg.contains("day"), "Expected 'day' field in round-trip: {pg}");
}

#[test]
fn functional_date_trunc_month_roundtrip() {
    let ddl = "CREATE TABLE events (id INT PRIMARY KEY, created_at TEXT);";
    let query = "SELECT date_trunc('month', created_at) FROM events;";
    let pg = forward_execute_reverse(ddl, query, &Pg2SqliteOptions::default());
    assert!(pg.contains("date_trunc"), "Expected date_trunc in round-trip: {pg}");
    assert!(pg.contains("month"), "Expected 'month' field in round-trip: {pg}");
}

#[test]
fn functional_date_trunc_year_roundtrip() {
    let ddl = "CREATE TABLE events (id INT PRIMARY KEY, created_at TEXT);";
    let query = "SELECT date_trunc('year', created_at) FROM events;";
    let pg = forward_execute_reverse(ddl, query, &Pg2SqliteOptions::default());
    assert!(pg.contains("date_trunc"), "Expected date_trunc in round-trip: {pg}");
    assert!(pg.contains("year"), "Expected 'year' field in round-trip: {pg}");
}

#[test]
fn functional_date_trunc_hour_roundtrip() {
    let ddl = "CREATE TABLE events (id INT PRIMARY KEY, created_at TEXT);";
    let query = "SELECT date_trunc('hour', created_at) FROM events;";
    let pg = forward_execute_reverse(ddl, query, &Pg2SqliteOptions::default());
    assert!(pg.contains("date_trunc"), "Expected date_trunc in round-trip: {pg}");
    assert!(pg.contains("hour"), "Expected 'hour' field in round-trip: {pg}");
}

#[test]
fn functional_date_trunc_minute_roundtrip() {
    let ddl = "CREATE TABLE events (id INT PRIMARY KEY, created_at TEXT);";
    let query = "SELECT date_trunc('minute', created_at) FROM events;";
    let pg = forward_execute_reverse(ddl, query, &Pg2SqliteOptions::default());
    assert!(pg.contains("date_trunc"), "Expected date_trunc in round-trip: {pg}");
    assert!(pg.contains("minute"), "Expected 'minute' field in round-trip: {pg}");
}

#[test]
fn functional_date_trunc_second_roundtrip() {
    let ddl = "CREATE TABLE events (id INT PRIMARY KEY, created_at TEXT);";
    let query = "SELECT date_trunc('second', created_at) FROM events;";
    let pg = forward_execute_reverse(ddl, query, &Pg2SqliteOptions::default());
    assert!(pg.contains("date_trunc"), "Expected date_trunc in round-trip: {pg}");
    assert!(pg.contains("second"), "Expected 'second' field in round-trip: {pg}");
}

#[test]
fn functional_to_timestamp_with_data() -> Result<(), Box<dyn std::error::Error>> {
    let pg_sql = "
        CREATE TABLE t (id INT PRIMARY KEY, epoch_val INT);
        SELECT to_timestamp(epoch_val) FROM t;
    ";
    let options = Pg2SqliteOptions::default();
    let translator = Pg2Sqlite::default().sql(pg_sql)?;
    let schema = translator.build_schema()?;
    let forward_stmts = translator.clone().translate(&options)?;

    let mut conn = SqliteConnection::establish(":memory:")?;

    for stmt in &forward_stmts {
        if !matches!(stmt, sqlparser::ast::Statement::Query(_)) {
            diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
        }
    }

    // Insert test data: Unix epoch 0 = 1970-01-01
    diesel::sql_query("INSERT INTO t (id, epoch_val) VALUES (1, 0), (2, 1700000000)")
        .execute(&mut conn)?;

    let select_stmt =
        forward_stmts.iter().find(|s| matches!(s, sqlparser::ast::Statement::Query(_))).unwrap();
    let sqlite_sql = select_stmt.to_string();

    #[derive(diesel::QueryableByName, Debug)]
    struct DateResult {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        result: Option<String>,
    }

    let results = diesel::sql_query(format!("SELECT ({sqlite_sql}) AS result FROM t"))
        .load::<DateResult>(&mut conn)?;
    assert_eq!(results.len(), 2);
    // Epoch 0 should produce 1970-01-01
    assert!(
        results[0].result.as_ref().unwrap().contains("1970-01-01"),
        "Epoch 0 should produce 1970-01-01, got: {:?}",
        results[0].result
    );

    // Now reverse the SQLite SQL and verify it parses as valid PostgreSQL
    let pg_stmts = translator.reverse_sql(&format!("{sqlite_sql};"), &schema, &options)?;
    let pg_sql = pg_stmts[0].to_string();
    Parser::parse_sql(&PostgreSqlDialect {}, &pg_sql)
        .unwrap_or_else(|e| panic!("Reverse output is not valid PostgreSQL: {e}\nSQL: {pg_sql}"));
    assert!(pg_sql.contains("to_timestamp"), "Expected to_timestamp: {pg_sql}");

    Ok(())
}

#[test]
fn functional_date_trunc_with_data() -> Result<(), Box<dyn std::error::Error>> {
    let pg_sql = "
        CREATE TABLE events (id INT PRIMARY KEY, created_at TEXT);
        SELECT date_trunc('day', created_at) FROM events;
    ";
    let options = Pg2SqliteOptions::default();
    let translator = Pg2Sqlite::default().sql(pg_sql)?;
    let schema = translator.build_schema()?;
    let forward_stmts = translator.clone().translate(&options)?;

    let mut conn = SqliteConnection::establish(":memory:")?;

    for stmt in &forward_stmts {
        if !matches!(stmt, sqlparser::ast::Statement::Query(_)) {
            diesel::sql_query(&stmt.to_string()).execute(&mut conn)?;
        }
    }

    diesel::sql_query(
        "INSERT INTO events (id, created_at) VALUES (1, '2024-03-15 10:30:00'), (2, '2024-03-15 23:59:59')",
    )
    .execute(&mut conn)?;

    let select_stmt =
        forward_stmts.iter().find(|s| matches!(s, sqlparser::ast::Statement::Query(_))).unwrap();
    let sqlite_sql = select_stmt.to_string();

    #[derive(diesel::QueryableByName, Debug)]
    struct TruncResult {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        truncated: Option<String>,
    }

    let results = diesel::sql_query(format!("SELECT ({sqlite_sql}) AS truncated FROM events"))
        .load::<TruncResult>(&mut conn)?;
    assert_eq!(results.len(), 2);
    // Both rows on the same day should produce the same day-truncated value
    assert_eq!(results[0].truncated, results[1].truncated);
    assert!(
        results[0].truncated.as_ref().unwrap().starts_with("2024-03-15"),
        "Expected day-truncated to 2024-03-15, got: {:?}",
        results[0].truncated
    );

    // Reverse and verify valid PostgreSQL
    let pg_stmts = translator.reverse_sql(&format!("{sqlite_sql};"), &schema, &options)?;
    let pg_sql = pg_stmts[0].to_string();
    Parser::parse_sql(&PostgreSqlDialect {}, &pg_sql)
        .unwrap_or_else(|e| panic!("Reverse output is not valid PostgreSQL: {e}\nSQL: {pg_sql}"));
    assert!(pg_sql.contains("date_trunc"), "Expected date_trunc: {pg_sql}");
    assert!(pg_sql.contains("day"), "Expected 'day': {pg_sql}");

    Ok(())
}
