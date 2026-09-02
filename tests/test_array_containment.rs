//! R2-16: array `@>` / `<@` containment operators.
//!
//! PostgreSQL array containment ignores multiplicity: `{1} @> {1,1}` is true.
//! The operators are distinct from jsonb containment: for declared array
//! columns under `ArrayRepresentation::Json` a `NOT EXISTS` / `json_each`
//! rewrite is faithful; for jsonb the refusal must name jsonb specifically.
//!
//! PostgreSQL ground truth measured on postgres:18-alpine:
//!   ARRAY[1,2,3] @> ARRAY[1,2]    -> true
//!   ARRAY[1,2,3] @> ARRAY[1,4]    -> false
//!   ARRAY[1,2]   @> ARRAY[1,1]    -> true  (duplicates ignored)
//!   ARRAY[1,2]   @> ARRAY[]::int[]-> true  (empty right side)
//!   ARRAY[1,2]   <@ ARRAY[1,2,3]  -> true
//!   ARRAY[3]     <@ ARRAY[1,2]    -> false

mod helpers;

use diesel::{QueryableByName, RunQueryDsl, prelude::*, sql_query, sql_types};
use helpers::establish_connection;
use pg2sqlite::prelude::{ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions};

fn opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json)
}

/// Translate a single PG statement with the array representation enabled.
fn tr(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&opts())
        .expect("translate")
        .into_iter()
        .next()
        .expect("at least one statement")
}

#[derive(QueryableByName, Debug)]
struct ScalarInt {
    #[diesel(sql_type = sql_types::Integer)]
    r: i32,
}

/// Result type for column-based containment queries that return `id`.
#[derive(QueryableByName, Debug)]
struct IdRow {
    #[diesel(sql_type = sql_types::Integer)]
    id: i32,
}

// --- literal array containment truths ----------------------------------------

/// `a @> b` is true when every element of b is in a (membership, not multiset).
#[test]
fn array_contains_literal_truthy() {
    // PG: ARRAY[1,2,3] @> ARRAY[1,2] -> true
    let sql = tr("SELECT (ARRAY[1,2,3] @> ARRAY[1,2])::int AS r");
    let mut conn = establish_connection();
    // sql_query: translated expression is dynamically generated, typed DSL
    // cannot express it.
    let rows = sql_query(&sql).load::<ScalarInt>(&mut conn).expect("execute containment");
    assert_eq!(rows[0].r, 1, "ARRAY[1,2,3] @> ARRAY[1,2] must be true: {sql}");
}

#[test]
fn array_contains_literal_falsy() {
    // PG: ARRAY[1,2] @> ARRAY[3] -> false
    let sql = tr("SELECT (ARRAY[1,2] @> ARRAY[3])::int AS r");
    let mut conn = establish_connection();
    // sql_query: translated expression is dynamically generated.
    let rows = sql_query(&sql).load::<ScalarInt>(&mut conn).expect("execute containment");
    assert_eq!(rows[0].r, 0, "ARRAY[1,2] @> ARRAY[3] must be false: {sql}");
}

/// Duplicates in the right operand are irrelevant - containment is set-based.
#[test]
fn array_contains_ignores_duplicates() {
    // PG: ARRAY[1,2] @> ARRAY[1,1] -> true (measured postgres:18-alpine)
    let sql = tr("SELECT (ARRAY[1,2] @> ARRAY[1,1])::int AS r");
    let mut conn = establish_connection();
    // sql_query: translated expression is dynamically generated.
    let rows = sql_query(&sql).load::<ScalarInt>(&mut conn).expect("execute containment");
    assert_eq!(rows[0].r, 1, "ARRAY[1,2] @> ARRAY[1,1] must be true (dup ignored): {sql}");
}

/// Every array contains an empty array.
#[test]
fn array_contains_empty_right_side_is_always_true() {
    // PG: ARRAY[1,2] @> ARRAY[]::int[] -> true (measured postgres:18-alpine)
    let sql = tr("SELECT (ARRAY[1,2] @> ARRAY[]::int[])::int AS r");
    let mut conn = establish_connection();
    // sql_query: translated expression is dynamically generated.
    let rows = sql_query(&sql).load::<ScalarInt>(&mut conn).expect("execute containment");
    assert_eq!(rows[0].r, 1, "ARRAY[1,2] @> ARRAY[] must be true: {sql}");
}

// --- <@ (contained-by) -------------------------------------------------------

#[test]
fn array_contained_by_literal_truthy() {
    // PG: ARRAY[1,2] <@ ARRAY[1,2,3] -> true
    let sql = tr("SELECT (ARRAY[1,2] <@ ARRAY[1,2,3])::int AS r");
    let mut conn = establish_connection();
    // sql_query: translated expression is dynamically generated.
    let rows = sql_query(&sql).load::<ScalarInt>(&mut conn).expect("execute contained-by");
    assert_eq!(rows[0].r, 1, "ARRAY[1,2] <@ ARRAY[1,2,3] must be true: {sql}");
}

#[test]
fn array_contained_by_literal_falsy() {
    // PG: ARRAY[1,3] <@ ARRAY[1,2] -> false
    let sql = tr("SELECT (ARRAY[1,3] <@ ARRAY[1,2])::int AS r");
    let mut conn = establish_connection();
    // sql_query: translated expression is dynamically generated.
    let rows = sql_query(&sql).load::<ScalarInt>(&mut conn).expect("execute contained-by");
    assert_eq!(rows[0].r, 0, "ARRAY[1,3] <@ ARRAY[1,2] must be false: {sql}");
}

// --- declared array column ---------------------------------------------------

// Schema for the column-based tests: a table with an int[] column.
diesel::table! {
    /// Test table with an int[] column, stored as JSON text under Json representation.
    arr_items (id) {
        /// Row identifier.
        id -> Integer,
        /// Array of integers stored as a JSON text array.
        tags -> Text,
    }
}

/// Rows for typed diesel inserts.
#[derive(Insertable)]
#[diesel(table_name = arr_items)]
struct NewArrItem {
    id: i32,
    tags: String,
}

fn apply_ddl(conn: &mut SqliteConnection, sql: &str) {
    // sql_query: applying translated DDL (dynamically generated, DSL cannot
    // express DDL).
    diesel::sql_query(sql).execute(conn).unwrap_or_else(|e| panic!("DDL failed: {e}\n{sql}"));
}

/// Build and populate an `arr_items` table through the translator, returning a
/// connection ready for containment queries.
fn arr_items_conn() -> SqliteConnection {
    let schema_and_query = "CREATE TABLE arr_items (id INT PRIMARY KEY, tags int[]);";
    let ddl: Vec<String> = Pg2Sqlite::default()
        .sql(schema_and_query)
        .expect("parse schema")
        .translate_to_sql(&opts())
        .expect("translate schema");

    let mut conn = establish_connection();
    for stmt in &ddl {
        apply_ddl(&mut conn, stmt);
    }

    // Use typed diesel insert for the known schema.
    diesel::insert_into(arr_items::table)
        .values(&[
            NewArrItem { id: 1, tags: "[1,2,3]".to_string() },
            NewArrItem { id: 2, tags: "[4,5,6]".to_string() },
            NewArrItem { id: 3, tags: "[1]".to_string() },
        ])
        .execute(&mut conn)
        .expect("insert arr_items");

    conn
}

/// Translate a SELECT over arr_items and return the translated SQL string.
fn tr_arr(select_pg: &str) -> String {
    let full = format!("CREATE TABLE arr_items (id INT PRIMARY KEY, tags int[]);\n{select_pg}");
    Pg2Sqlite::default()
        .sql(&full)
        .expect("parse")
        .translate_to_sql(&opts())
        .expect("translate")
        .into_iter()
        .last()
        .expect("at least one statement")
}

#[test]
fn array_column_containment_finds_matching_rows() {
    let mut conn = arr_items_conn();

    // Translate the containment query against the declared schema.
    let sql = tr_arr("SELECT id FROM arr_items WHERE tags @> ARRAY[1,2]");

    // sql_query: running dynamically generated translated SQL.
    let rows = sql_query(&sql).load::<IdRow>(&mut conn).expect("execute column containment");
    let ids: Vec<i32> = rows.iter().map(|r| r.id).collect();
    // Row 1 has [1,2,3] which contains [1,2]; row 3 has [1] which does not.
    assert_eq!(ids, vec![1], "only row 1 contains [1,2]: {sql}");
}

#[test]
fn array_column_contained_by_finds_matching_rows() {
    let mut conn = arr_items_conn();

    // tags <@ ARRAY[1,2,3]: rows where every element of tags is in [1,2,3].
    let sql = tr_arr("SELECT id FROM arr_items WHERE tags <@ ARRAY[1,2,3]");

    // sql_query: running dynamically generated translated SQL.
    let rows = sql_query(&sql).load::<IdRow>(&mut conn).expect("execute column contained-by");
    let mut ids: Vec<i32> = rows.iter().map(|r| r.id).collect();
    ids.sort_unstable();
    // Row 1 has [1,2,3] subset of [1,2,3]; row 3 has [1] subset of [1,2,3].
    // Row 2 has [4,5,6] not subset.
    assert_eq!(ids, vec![1, 3], "rows 1 and 3 have tags contained in [1,2,3]: {sql}");
}

// --- jsonb refusal still names jsonb -----------------------------------------

/// The existing jsonb-containment refusal message must name jsonb specifically.
/// Previously both array and jsonb cases shared the same generic message; after
/// the fix, the jsonb path says "jsonb" and the array path succeeds.
#[test]
fn jsonb_at_arrow_refusal_names_jsonb() {
    // Use default opts (no array representation) so both operands look like
    // jsonb.
    let err = Pg2Sqlite::default()
        .sql("SELECT '{\"a\":1}'::jsonb @> '{}'::jsonb")
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("jsonb containment must be refused")
        .to_string();
    assert!(
        err.to_lowercase().contains("jsonb"),
        "refusal for jsonb @> must name jsonb, got: {err}"
    );
}

#[test]
fn jsonb_arrow_at_refusal_names_jsonb() {
    let err = Pg2Sqlite::default()
        .sql("SELECT '{}'::jsonb <@ '{\"a\":1}'::jsonb")
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("jsonb contained-by must be refused")
        .to_string();
    assert!(
        err.to_lowercase().contains("jsonb"),
        "refusal for jsonb <@ must name jsonb, got: {err}"
    );
}
