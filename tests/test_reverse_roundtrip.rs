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

use diesel::{Connection, ExpressionMethods, RunQueryDsl, SqliteConnection};
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

diesel::table! {
    /// Sensor readings, seeded through the typed DSL so the only raw SQL in
    /// these tests is the translator output under test.
    readings (id) {
        /// Primary key.
        id -> Integer,
        /// Sensor name, used as the `DISTINCT ON` partition key.
        sensor -> Text,
        /// Reading timestamp.
        ts -> Integer,
        /// Reading value.
        value -> Integer,
    }
}

const READINGS: &str = "CREATE TABLE readings (id INT PRIMARY KEY, sensor TEXT NOT NULL, \
                        ts INT NOT NULL, value INT NOT NULL);";

const DISTINCT_ON_QUERY: &str =
    "SELECT DISTINCT ON (sensor) sensor, ts, value FROM readings ORDER BY sensor, ts DESC;";

#[derive(diesel::QueryableByName, Debug, PartialEq, Eq)]
struct Reading {
    #[diesel(sql_type = diesel::sql_types::Text)]
    sensor: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    ts: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    value: i32,
}

#[derive(diesel::QueryableByName, Debug, PartialEq, Eq)]
struct LatestReading {
    #[diesel(sql_type = diesel::sql_types::Text)]
    sensor: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    latest: i32,
}

/// Forward translates [`READINGS`] plus `pg_query` and returns the emitted
/// SELECT alongside a SQLite database holding the emitted schema and a seed.
fn forward_and_seed(pg_query: &str) -> (String, SqliteConnection) {
    let options = Pg2SqliteOptions::default();
    let translator =
        Pg2Sqlite::default().sql(&format!("{READINGS}\n{pg_query}")).expect("script should parse");
    let statements = translator.translate(&options).expect("script should translate");

    let mut connection =
        SqliteConnection::establish(":memory:").expect("in-memory SQLite should open");
    for statement in &statements {
        if !matches!(statement, sqlparser::ast::Statement::Query(_)) {
            // Emitted DDL is the artifact under test, so it runs as text.
            diesel::sql_query(statement.to_string())
                .execute(&mut connection)
                .unwrap_or_else(|error| panic!("emitted DDL failed: {error}\n{statement}"));
        }
    }

    diesel::insert_into(readings::table)
        .values(vec![
            (
                readings::id.eq(1),
                readings::sensor.eq("a"),
                readings::ts.eq(10),
                readings::value.eq(100),
            ),
            (
                readings::id.eq(2),
                readings::sensor.eq("a"),
                readings::ts.eq(30),
                readings::value.eq(300),
            ),
            (
                readings::id.eq(3),
                readings::sensor.eq("b"),
                readings::ts.eq(20),
                readings::value.eq(200),
            ),
            (
                readings::id.eq(4),
                readings::sensor.eq("b"),
                readings::ts.eq(5),
                readings::value.eq(50),
            ),
        ])
        .execute(&mut connection)
        .expect("seed should insert");

    let select = statements
        .iter()
        .find(|statement| matches!(statement, sqlparser::ast::Statement::Query(_)))
        .expect("script should emit a SELECT")
        .to_string();
    (select, connection)
}

/// Runs `select`, which is translator output rather than a query this suite
/// composes, so it goes to SQLite as text.
fn run_select<R>(select: &str, connection: &mut SqliteConnection) -> Vec<R>
where
    R: diesel::deserialize::QueryableByName<diesel::sqlite::Sqlite> + 'static,
{
    diesel::sql_query(select)
        .load::<R>(connection)
        .unwrap_or_else(|error| panic!("emitted SELECT failed: {error}\n{select}"))
}

/// The `DISTINCT ON` expressions of `pg_sql`, read from the parsed tree rather
/// than from its text, which also proves the reverse output parses.
fn distinct_on_targets(pg_sql: &str) -> Vec<String> {
    let statements = Parser::parse_sql(&PostgreSqlDialect {}, pg_sql)
        .unwrap_or_else(|error| panic!("not valid PostgreSQL: {error}\n{pg_sql}"));
    let Some(sqlparser::ast::Statement::Query(query)) = statements.first() else {
        panic!("expected a single query, got {pg_sql}");
    };
    let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
        panic!("expected a SELECT, got {pg_sql}");
    };
    match select.distinct.as_ref() {
        Some(sqlparser::ast::Distinct::On(exprs)) => {
            exprs.iter().map(ToString::to_string).collect()
        }
        _ => Vec::new(),
    }
}

fn reading(sensor: &str, ts: i32, value: i32) -> Reading {
    Reading { sensor: sensor.to_string(), ts, value }
}

#[test]
fn distinct_on_round_trips_through_the_row_number_rewrite() {
    let (sqlite_sql, mut connection) = forward_and_seed(DISTINCT_ON_QUERY);
    assert_eq!(
        run_select::<Reading>(&sqlite_sql, &mut connection),
        vec![reading("a", 30, 300), reading("b", 20, 200)],
        "the rewrite should keep the highest ts per sensor: {sqlite_sql}"
    );

    let pg_sql = reverse(READINGS, &format!("{sqlite_sql};"));
    assert_eq!(
        distinct_on_targets(&pg_sql),
        vec!["sensor".to_string()],
        "reversing the rewrite should restore DISTINCT ON (sensor): {pg_sql}"
    );

    let (round_tripped, _) = forward_and_seed(&format!("{pg_sql};"));
    assert_eq!(
        round_tripped, sqlite_sql,
        "the restored query should translate back to the same SQLite"
    );
}

/// A `DISTINCT ON` with no `ORDER BY` leaves the window unordered too, which
/// reaches a different branch of the reconstruction.
#[test]
fn distinct_on_without_an_order_by_round_trips() {
    let (sqlite_sql, mut connection) =
        forward_and_seed("SELECT DISTINCT ON (sensor) sensor, ts, value FROM readings;");
    let mut sensors = run_select::<Reading>(&sqlite_sql, &mut connection)
        .into_iter()
        .map(|row| row.sensor)
        .collect::<Vec<_>>();
    sensors.sort();
    assert_eq!(sensors, vec!["a".to_string(), "b".to_string()], "one row per sensor: {sqlite_sql}");

    let pg_sql = reverse(READINGS, &format!("{sqlite_sql};"));
    assert_eq!(
        distinct_on_targets(&pg_sql),
        vec!["sensor".to_string()],
        "an unordered rewrite should still restore DISTINCT ON: {pg_sql}"
    );

    let (round_tripped, _) = forward_and_seed(&format!("{pg_sql};"));
    assert_eq!(round_tripped, sqlite_sql);
}

/// The rewrite names every projected column, so the reconstruction drops an
/// alias that only repeats a column name. An alias the author wrote stays, and
/// loading by the emitted column name is what proves it.
///
/// No `ORDER BY` here, because the rewrite cannot yet order a projection that
/// renames a column. See R90.
#[test]
fn a_projection_alias_survives_the_round_trip() {
    let (sqlite_sql, mut connection) =
        forward_and_seed("SELECT DISTINCT ON (sensor) sensor, value AS latest FROM readings;");
    let mut rows = run_select::<LatestReading>(&sqlite_sql, &mut connection);
    rows.sort_by(|left, right| left.sensor.cmp(&right.sensor));
    assert_eq!(
        rows.iter().map(|row| row.sensor.as_str()).collect::<Vec<_>>(),
        vec!["a", "b"],
        "one row per sensor, projected as latest: {sqlite_sql}"
    );

    let pg_sql = reverse(READINGS, &format!("{sqlite_sql};"));
    let (round_tripped, _) = forward_and_seed(&format!("{pg_sql};"));
    assert_eq!(round_tripped, sqlite_sql, "the written alias should survive: {pg_sql}");
}

/// Guards the reconstruction from firing on any window filter. A hand written
/// one carries no marker aliases and must stay a window filter.
#[test]
fn a_window_filter_without_the_markers_stays_a_window_filter() {
    let sqlite_sql = "SELECT ranked.sensor, ranked.ts FROM (SELECT sensor AS sensor, ts AS ts, \
                      ROW_NUMBER() OVER (PARTITION BY sensor) AS rn FROM readings) ranked \
                      WHERE ranked.rn = 1;";
    let pg_sql = reverse(READINGS, sqlite_sql);
    assert!(
        distinct_on_targets(&pg_sql).is_empty(),
        "an unmarked window filter must not become DISTINCT ON: {pg_sql}"
    );
}

/// Guards the reconstruction against a marker shape whose window ordering
/// disagrees with the query ordering. `DISTINCT ON` keeps the first row of the
/// query ordering, so rebuilding it there would pick a different row.
#[test]
fn a_marker_shape_with_a_divergent_window_order_stays_a_window_filter() {
    let sqlite_sql = "SELECT __pg2sqlite_distinct_on.sensor, __pg2sqlite_distinct_on.ts \
                      FROM (SELECT sensor AS sensor, ts AS ts, ROW_NUMBER() OVER (PARTITION BY \
                      sensor ORDER BY ts DESC) AS __pg2sqlite_rn FROM readings) \
                      __pg2sqlite_distinct_on WHERE __pg2sqlite_distinct_on.__pg2sqlite_rn = 1 \
                      ORDER BY sensor, ts ASC;";
    let pg_sql = reverse(READINGS, sqlite_sql);
    assert!(
        distinct_on_targets(&pg_sql).is_empty(),
        "a divergent window order must not be rebuilt as DISTINCT ON: {pg_sql}"
    );
}

/// PostgreSQL refuses a `DISTINCT ON` whose expressions are not the initial
/// `ORDER BY` expressions, so a marker shape that would produce one is left
/// alone rather than reversed into a query that cannot run.
#[test]
fn a_marker_shape_that_breaks_the_order_by_rule_stays_a_window_filter() {
    let sqlite_sql = "SELECT __pg2sqlite_distinct_on.sensor, __pg2sqlite_distinct_on.ts \
                      FROM (SELECT sensor AS sensor, ts AS ts, ROW_NUMBER() OVER (PARTITION BY \
                      sensor ORDER BY ts DESC) AS __pg2sqlite_rn FROM readings) \
                      __pg2sqlite_distinct_on WHERE __pg2sqlite_distinct_on.__pg2sqlite_rn = 1 \
                      ORDER BY ts DESC;";
    let pg_sql = reverse(READINGS, sqlite_sql);
    assert!(
        distinct_on_targets(&pg_sql).is_empty(),
        "DISTINCT ON (sensor) under ORDER BY ts would not run in PostgreSQL: {pg_sql}"
    );
}
