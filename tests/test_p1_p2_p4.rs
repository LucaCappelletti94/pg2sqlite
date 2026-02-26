//! TDD tests for P1, P2, and P4 improvements.
//!
//! Each test is written RED-first (verifying the fix is in place) and exercises
//! the translated SQL against a real SQLite database via Diesel to confirm
//! runtime correctness.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};

// ---------------------------------------------------------------------------
// P1.8: bigserial / largeserial must map to INTEGER
// ---------------------------------------------------------------------------

/// `bigserial` is PostgreSQL shorthand for `BIGINT NOT NULL DEFAULT
/// nextval(…)`. The translator must map it to `INTEGER` (the only integer type
/// in SQLite).
#[test]
fn bigserial_translates_to_integer() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE bs_test (id BIGSERIAL PRIMARY KEY, val TEXT NOT NULL);";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let ddl = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");
    assert!(ddl.to_uppercase().contains("INTEGER"), "bigserial must become INTEGER, got: {ddl}");

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(())
}

/// `largeserial` is an alias for `bigserial` in some PostgreSQL docs.
#[test]
fn largeserial_translates_to_integer() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE ls_test (id LARGESERIAL PRIMARY KEY, val TEXT NOT NULL);";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let ddl = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");
    assert!(ddl.to_uppercase().contains("INTEGER"), "largeserial must become INTEGER, got: {ddl}");

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// P1.12: INT(11) / SMALLINT(6) must drop precision and map to INTEGER
// ---------------------------------------------------------------------------

/// `INT(11)` is MySQL-style syntax where the number is a display-width hint.
/// SQLite has no display-width concept; the precision must be dropped.
#[test]
fn int_with_precision_translates_to_integer() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE int_prec_test (id INTEGER PRIMARY KEY, val INT(11) NOT NULL);";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let ddl = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");
    // Must not contain INT(11) — precision stripped
    assert!(
        !ddl.contains("INT(11)") && !ddl.contains("INT("),
        "INT(11) precision must be dropped, got: {ddl}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(())
}

/// `SMALLINT(6)` — display-width hint must also be dropped.
#[test]
fn smallint_with_precision_translates_to_integer() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE si_test (id INTEGER PRIMARY KEY, flag SMALLINT(6) NOT NULL);";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let ddl = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");
    assert!(!ddl.contains("SMALLINT("), "SMALLINT(6) precision must be dropped, got: {ddl}");

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// P1.7: DEFAULT now() must translate to DEFAULT datetime('now')
// ---------------------------------------------------------------------------

/// PostgreSQL's `now()` function returns the current timestamp.
/// SQLite's equivalent is `datetime('now')`.
/// A column with `DEFAULT now()` must survive translation and work at runtime.
#[test]
fn default_now_translates_to_datetime_now() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE now_test (id INTEGER PRIMARY KEY, created_at TEXT DEFAULT now());";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let ddl = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");
    assert!(
        ddl.contains("datetime") || ddl.contains("DATETIME"),
        "now() must become datetime('now'), got: {ddl}"
    );
    assert!(
        !ddl.to_lowercase().contains("now()"),
        "now() must be rewritten, not passed through, got: {ddl}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    // Insert without specifying created_at to exercise the DEFAULT.
    diesel::sql_query("INSERT INTO now_test (id) VALUES (1)").execute(&mut conn)?;

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        created_at: Option<String>,
    }
    let rows = diesel::sql_query("SELECT created_at FROM now_test").load::<Row>(&mut conn)?;
    assert_eq!(rows.len(), 1, "Should have one row");
    assert!(rows[0].created_at.is_some(), "DEFAULT datetime('now') should set a timestamp");
    Ok(())
}

// ---------------------------------------------------------------------------
// P1.9: INTERVAL expressions must cause a clear error
// ---------------------------------------------------------------------------

/// SQLite does not support INTERVAL expressions.
/// `WHERE ts > INTERVAL '7 days'` would be invalid SQLite and must be rejected.
#[test]
fn interval_expression_causes_error() {
    let sql = "CREATE TABLE iv_test (id INTEGER PRIMARY KEY, ts TEXT);
               SELECT * FROM iv_test WHERE ts > INTERVAL '7 days';";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "INTERVAL expression must cause a translation error");
    let err = result.unwrap_err().to_string().to_lowercase();
    assert!(err.contains("interval"), "Error must mention INTERVAL, got: {err}");
}

/// Suggest using SQLite date modifiers instead of INTERVAL.
#[test]
fn interval_error_suggests_date_modifier() {
    let sql = "CREATE TABLE iv2_test (id INTEGER PRIMARY KEY, ts TEXT);
               SELECT * FROM iv2_test WHERE ts > INTERVAL '1 hour';";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    let err = result.unwrap_err().to_string().to_lowercase();
    assert!(
        err.contains("date") || err.contains("datetime"),
        "Error should suggest SQLite date functions, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// P1.11: COLLATE with PostgreSQL collation names must cause a clear error
// ---------------------------------------------------------------------------

/// SQLite only knows BINARY, NOCASE, and RTRIM.
/// PostgreSQL collation names like "en_US" or "C" are invalid in SQLite.
#[test]
fn pg_collation_name_causes_error() {
    let sql = "CREATE TABLE coll_test (id INTEGER PRIMARY KEY, name TEXT);
               SELECT * FROM coll_test ORDER BY name COLLATE \"en_US\";";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "Non-SQLite collation must cause a translation error");
    let err = result.unwrap_err().to_string().to_lowercase();
    assert!(
        err.contains("collat") || err.contains("binary") || err.contains("nocase"),
        "Error must mention SQLite collation names, got: {err}"
    );
}

/// SQLite NOCASE collation must still work after the validation is in place.
#[test]
fn nocase_collation_still_works() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE nc_test (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
               SELECT * FROM nc_test ORDER BY name COLLATE NOCASE;";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();
    assert!(
        select_sql.to_uppercase().contains("NOCASE"),
        "NOCASE must be preserved, got: {select_sql}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// P4.27: translate_to_sql convenience method
// ---------------------------------------------------------------------------

/// `translate_to_sql` must return `Vec<String>` of SQL text directly.
#[test]
fn translate_to_sql_returns_strings() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE convenience_test (id INTEGER PRIMARY KEY, val TEXT);";
    let sql_strings =
        Pg2Sqlite::default().sql(sql)?.translate_to_sql(&Pg2SqliteOptions::default())?;
    assert!(!sql_strings.is_empty(), "translate_to_sql must return at least one statement");
    assert!(
        sql_strings[0].to_uppercase().contains("CREATE TABLE"),
        "First string must be the CREATE TABLE, got: {}",
        sql_strings[0]
    );

    // The returned strings must be valid SQLite.
    let mut conn = SqliteConnection::establish(":memory:")?;
    for sql_str in &sql_strings {
        diesel::sql_query(sql_str).execute(&mut conn)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// P4.28: with_uuid_function_name accepts &str (impl Into<String>)
// ---------------------------------------------------------------------------

/// The API must accept a string literal (&str), not require `.to_string()`.
#[test]
fn uuid_function_name_accepts_str_literal() -> Result<(), Box<dyn std::error::Error>> {
    use pg2sqlite::prelude::UuidRepresentation;
    // This should compile with a &str literal, not requiring String::from(...)
    let options = Pg2SqliteOptions::default()
        .with_uuid_representation(UuidRepresentation::Text)
        .with_uuid_function_name("my_uuid_fn"); // &str, not String
    // Use gen_random_uuid() as DEFAULT so the function name appears in output.
    let sql = "CREATE TABLE uuid_str_test (id UUID PRIMARY KEY DEFAULT gen_random_uuid(), name TEXT NOT NULL);";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&options)?;
    let ddl = translated.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n");
    assert!(
        ddl.contains("my_uuid_fn"),
        "Custom UUID function name must appear in DEFAULT, got: {ddl}"
    );
    Ok(())
}

/// The EXTRACT(SECOND) translation must produce runnable SQLite.
#[test]
fn extract_second_produces_valid_sqlite() -> Result<(), Box<dyn std::error::Error>> {
    let sql = "CREATE TABLE sec2_test (id INTEGER PRIMARY KEY, ts TEXT NOT NULL);
               SELECT EXTRACT(SECOND FROM ts) AS secs FROM sec2_test;";
    let translated = Pg2Sqlite::default().sql(sql)?.translate(&Pg2SqliteOptions::default())?;
    let select_sql = translated
        .iter()
        .find(|s| matches!(s, sqlparser::ast::Statement::Query(_)))
        .unwrap()
        .to_string();
    assert!(
        select_sql.to_lowercase().contains("strftime"),
        "EXTRACT(SECOND) must use strftime, got: {select_sql}"
    );

    let mut conn = SqliteConnection::establish(":memory:")?;
    for stmt in &translated {
        diesel::sql_query(stmt.to_string()).execute(&mut conn)?;
    }
    diesel::sql_query("INSERT INTO sec2_test VALUES (1, '2024-01-15 10:30:45.123')")
        .execute(&mut conn)?;

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Double>)]
        secs: Option<f64>,
    }
    let rows = diesel::sql_query(select_sql).load::<Row>(&mut conn)?;
    assert!(rows[0].secs.is_some(), "Should have extracted seconds");
    let secs = rows[0].secs.unwrap();
    assert!((secs - 45.123).abs() < 0.001, "Expected ~45.123 seconds, got {secs}");
    Ok(())
}
