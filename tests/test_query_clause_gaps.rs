//! Query pagination, nested query scope and refusal contracts.

use diesel::{
    connection::SimpleConnection,
    prelude::*,
    sql_types::{Integer, Nullable},
};
use pg2sqlite::{
    errors::{Error, TranslationDirection},
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};
use sqlparser::ast::Statement;

diesel::table! {
    /// Ordered pagination fixtures.
    nums (n) {
        /// A distinct integer value.
        n -> Integer,
    }
}

const SCHEMA: &str = "CREATE TABLE nums (n INTEGER NOT NULL);";

#[derive(QueryableByName)]
struct Number {
    #[diesel(sql_type = Integer)]
    n: i32,
}

#[derive(QueryableByName)]
struct MaybeNumber {
    #[diesel(sql_type = Nullable<Integer>)]
    n: Option<i32>,
}

fn translated(source: &str) -> Result<Vec<Statement>, Error> {
    let schema = Pg2Sqlite::default().sql(SCHEMA)?.build_schema()?;
    Pg2Sqlite::default().sql(source)?.translate_with_schema(&schema, &Pg2SqliteOptions::default())
}

fn query(source: &str) -> String {
    translated(source)
        .unwrap()
        .into_iter()
        .find_map(|statement| {
            match statement {
                Statement::Query(query) => Some(query.to_string()),
                _ => None,
            }
        })
        .expect("translated query")
}

fn fixture() -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    connection.batch_execute(SCHEMA).unwrap();
    let rows = [1, 2, 3, 4, 5].map(|n| nums::n.eq(n));
    diesel::insert_into(nums::table).values(&rows).execute(&mut connection).unwrap();
    connection
}

fn assert_rows(source: &str, expected: &[i32]) {
    // Translated SQL is runtime syntax under test.
    let actual = diesel::sql_query(query(source))
        .load::<Number>(&mut fixture())
        .unwrap()
        .into_iter()
        .map(|row| row.n)
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "{source}");
}

fn assert_refusal(source: &str) {
    let Error::TranslationRefusal(refusal) = translated(source).expect_err("translation refusal")
    else {
        panic!("expected a structured translation refusal");
    };
    assert_eq!(refusal.direction(), TranslationDirection::PostgreSqlToSqlite);
}

#[test]
fn fetch_first_rows_only_preserves_selection() {
    assert_rows("SELECT n FROM nums ORDER BY n FETCH FIRST 3 ROWS ONLY", &[1, 2, 3]);
}

#[test]
fn offset_fetch_preserves_selection() {
    assert_rows("SELECT n FROM nums ORDER BY n OFFSET 2 ROWS FETCH FIRST 3 ROWS ONLY", &[3, 4, 5]);
}

#[test]
fn fetch_with_ties_is_refused() {
    assert_refusal("SELECT n FROM nums ORDER BY n FETCH FIRST 3 ROWS WITH TIES");
}

#[test]
fn uncorrelated_lateral_preserves_values() {
    assert_rows(
        "SELECT nums.n FROM nums, LATERAL (SELECT 1) AS lat ORDER BY nums.n",
        &[1, 2, 3, 4, 5],
    );
}

#[test]
fn correlated_lateral_is_refused() {
    assert_refusal("SELECT nums.n FROM nums, LATERAL (SELECT nums.n) AS lat");
}

#[test]
fn intersect_all_is_refused() {
    assert_refusal("SELECT n FROM nums INTERSECT ALL SELECT n FROM nums");
}

#[test]
fn bare_offset_preserves_selection() {
    assert_rows("SELECT n FROM nums ORDER BY n OFFSET 2", &[3, 4, 5]);
}

#[test]
fn implicit_fetch_quantity_selects_one_row() {
    for spelling in ["FIRST ROW", "NEXT ROW", "FIRST ROWS", "NEXT ROWS"] {
        assert_rows(&format!("SELECT n FROM nums ORDER BY n FETCH {spelling} ONLY"), &[1]);
    }
}

#[test]
fn implicit_fetch_quantity_with_offset_selects_one_row() {
    assert_rows("SELECT n FROM nums ORDER BY n OFFSET 2 ROWS FETCH FIRST ROW ONLY", &[3]);
    assert_rows("SELECT n FROM nums ORDER BY n OFFSET 5 ROWS FETCH FIRST ROW ONLY", &[]);
}

#[test]
fn implicit_fetch_quantity_in_cte_selects_one_row() {
    assert_rows(
        "WITH selected AS (SELECT n FROM nums ORDER BY n FETCH FIRST ROW ONLY) SELECT n FROM selected",
        &[1],
    );
    assert_rows(
        "WITH selected AS (SELECT n FROM nums) SELECT n FROM selected ORDER BY n OFFSET 1 ROW FETCH NEXT ROW ONLY",
        &[2],
    );
}

#[test]
fn implicit_fetch_quantity_in_derived_table_selects_one_row() {
    assert_rows(
        "SELECT n FROM (SELECT n FROM nums ORDER BY n OFFSET 1 ROW FETCH FIRST ROW ONLY) AS paged",
        &[2],
    );
}

#[test]
fn implicit_fetch_quantity_in_scalar_subquery_selects_one_row() {
    let source = "SELECT (SELECT n FROM nums ORDER BY n OFFSET 2 ROWS FETCH FIRST ROW ONLY) AS n";
    // The scalar query and its nullable result are runtime syntax under test.
    let actual =
        diesel::sql_query(query(source)).get_result::<MaybeNumber>(&mut fixture()).unwrap();
    assert_eq!(actual.n, Some(3));
}

#[test]
fn explicit_fetch_quantity_preserves_zero_and_nonzero() {
    assert_rows("SELECT n FROM nums ORDER BY n FETCH FIRST 0 ROWS ONLY", &[]);
    assert_rows("SELECT n FROM nums ORDER BY n OFFSET 1 ROW FETCH FIRST 2 ROWS ONLY", &[2, 3]);
    assert_rows("SELECT n FROM nums ORDER BY n OFFSET 4 ROW FETCH FIRST 10 ROWS ONLY", &[5]);
}

#[test]
fn implicit_fetch_quantity_preserves_bound_offset() {
    let source = "SELECT n FROM nums ORDER BY n OFFSET $1 ROWS FETCH FIRST ROW ONLY";
    // Translated SQL and positional parameters are runtime syntax under test.
    let actual = diesel::sql_query(query(source))
        .bind::<Integer, _>(2)
        .load::<Number>(&mut fixture())
        .unwrap()
        .into_iter()
        .map(|row| row.n)
        .collect::<Vec<_>>();
    assert_eq!(actual, [3]);
}

#[test]
fn explicit_fetch_preserves_bind_identity() {
    let source = "SELECT n FROM nums ORDER BY n OFFSET $1 ROWS FETCH FIRST $2 ROWS ONLY";
    // Translated SQL and positional parameters are runtime syntax under test.
    let actual = diesel::sql_query(query(source))
        .bind::<Integer, _>(1)
        .bind::<Integer, _>(3)
        .load::<Number>(&mut fixture())
        .unwrap()
        .into_iter()
        .map(|row| row.n)
        .collect::<Vec<_>>();
    assert_eq!(actual, [2, 3, 4]);
}

#[test]
fn fetch_percent_is_refused() {
    assert_refusal("SELECT n FROM nums ORDER BY n FETCH FIRST 20 PERCENT ROWS ONLY");
}

#[test]
fn alias_column_list_is_refused() {
    assert_refusal("SELECT a FROM (VALUES (1),(2)) AS v(a)");
}

#[test]
fn tablesample_is_refused() {
    assert_refusal("SELECT n FROM nums TABLESAMPLE BERNOULLI(10)");
}

#[test]
fn nested_query_dispatch_applies_pagination() {
    assert_rows(
        "SELECT n FROM ((SELECT n FROM nums ORDER BY n OFFSET 1 ROW FETCH FIRST 2 ROWS ONLY)) AS paged ORDER BY n",
        &[2, 3],
    );
    assert_rows(
        "SELECT n FROM ((SELECT n FROM nums ORDER BY n OFFSET 3)) AS paged ORDER BY n",
        &[4, 5],
    );
    assert_rows("SELECT ((SELECT n FROM nums ORDER BY n FETCH FIRST 1 ROW ONLY)) AS n", &[1]);
}

#[test]
fn compound_operand_clauses_stay_local() {
    assert_rows(
        "(SELECT n FROM nums ORDER BY n FETCH FIRST 2 ROWS ONLY) UNION ALL (SELECT n FROM nums ORDER BY n DESC OFFSET 1 ROW FETCH NEXT 2 ROWS ONLY) ORDER BY n",
        &[1, 2, 3, 4],
    );
    assert_rows(
        "SELECT n FROM nums WHERE n < 3 UNION ALL (SELECT n FROM nums ORDER BY n OFFSET 3) ORDER BY n",
        &[1, 2, 4, 5],
    );
}

#[test]
fn cte_compound_operand_dispatch_applies_pagination() {
    assert_rows(
        "WITH selected AS ((SELECT n FROM nums ORDER BY n FETCH FIRST 2 ROWS ONLY) UNION ALL SELECT n FROM nums WHERE false) SELECT n FROM selected ORDER BY n",
        &[1, 2],
    );
}

#[test]
fn nested_query_scopes_preserve_cte_shadowing() {
    assert_rows(
        "SELECT n FROM ((WITH nums AS (SELECT 11 AS n UNION ALL SELECT 13 AS n) SELECT n FROM nums ORDER BY n FETCH FIRST 1 ROW ONLY)) AS paged",
        &[11],
    );
    assert_rows(
        "WITH nums AS (SELECT 11 AS n UNION ALL SELECT 13 AS n) SELECT n FROM (SELECT n FROM nums ORDER BY n FETCH FIRST 1 ROW ONLY) AS paged",
        &[11],
    );
}

#[test]
fn inherited_cte_types_shadow_schema_columns() {
    assert_rows(
        "WITH nums AS (SELECT 'abcd'::text AS n) SELECT (SELECT char_length(n) FROM nums) AS n",
        &[4],
    );
    assert_rows(
        "WITH nums AS (SELECT 'abcd'::text AS n) SELECT n FROM (SELECT char_length(n) AS n FROM nums) AS measured",
        &[4],
    );
}

#[test]
fn nested_query_dispatch_preserves_refusals() {
    assert_refusal(
        "SELECT n FROM ((SELECT n FROM nums ORDER BY n FETCH FIRST 2 ROWS WITH TIES)) AS paged",
    );
}

#[test]
fn nested_row_locks_report_lossy_semantics() {
    let source = "SELECT n FROM ((SELECT n FROM nums FOR UPDATE)) AS locked ORDER BY n";
    assert_rows(source, &[1, 2, 3, 4, 5]);
    let schema = Pg2Sqlite::default().sql(SCHEMA).unwrap().build_schema().unwrap();
    let report = Pg2Sqlite::default()
        .sql(source)
        .unwrap()
        .translate_with_report_and_schema(&schema, &Pg2SqliteOptions::default())
        .unwrap();
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|warning| {
                matches!(warning, pg2sqlite::warnings::TranslationWarning::LossyDrop { .. })
            })
            .count(),
        1,
    );
}

#[test]
fn nested_operand_depth_preserves_pagination() {
    for depth in [1, 2, 4, 8] {
        let operand = format!(
            "{}SELECT n FROM nums ORDER BY n FETCH FIRST 2 ROWS ONLY{}",
            "(".repeat(depth),
            ")".repeat(depth),
        );
        assert_rows(&format!("{operand} UNION SELECT 5 AS n ORDER BY n"), &[1, 2, 5]);
    }
}

#[test]
fn nested_query_dispatch_preserves_dml_state() {
    for (source, expected) in [
        (
            "INSERT INTO nums(n) SELECT n FROM ((SELECT n FROM nums ORDER BY n FETCH FIRST 1 ROW ONLY)) AS paged",
            &[1, 1, 2, 3, 4, 5][..],
        ),
        (
            "UPDATE nums SET n=((SELECT n FROM nums ORDER BY n DESC FETCH FIRST 1 ROW ONLY)) WHERE n=1",
            &[2, 3, 4, 5, 5][..],
        ),
        (
            "DELETE FROM nums WHERE n<=((SELECT n FROM nums ORDER BY n OFFSET 1 ROW FETCH FIRST 1 ROW ONLY))",
            &[3, 4, 5][..],
        ),
    ] {
        let mut connection = fixture();
        for statement in translated(source).unwrap() {
            // Translated DML and its query source are runtime syntax under
            // test.
            connection.batch_execute(&statement.to_string()).unwrap();
        }
        let actual =
            nums::table.select(nums::n).order(nums::n.asc()).load::<i32>(&mut connection).unwrap();
        assert_eq!(actual, expected, "{source}");
    }
}
