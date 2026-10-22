//! Operands a lowering names more than once, with translated SQL run raw as the
//! output under test.

use diesel::{
    QueryableByName, connection::SimpleConnection, prelude::*, sql_query, sql_types::BigInt,
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        nums (id) {
            id -> Integer,
            a -> Integer,
            b -> Integer,
        }
    }
}
use schema::nums;

const TABLE: &str = "CREATE TABLE nums (id INT PRIMARY KEY, a INT NOT NULL, b INT NOT NULL);";

#[derive(QueryableByName)]
struct Row {
    #[diesel(sql_type = BigInt)]
    r: i64,
}

fn translate(pg: &str, options: &Pg2SqliteOptions) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(options)
        .expect("translate")
        .into_vec()
        .into_iter()
        .filter(|statement| !statement.starts_with("PRAGMA"))
        .collect()
}

fn loaded(connection: &mut SqliteConnection, statements: &[String]) -> Vec<i64> {
    let (query, definitions) = statements.split_last().expect("a query");
    for definition in definitions {
        connection.batch_execute(definition).expect("apply the definition");
    }
    let rows = [(1, 5, 3), (2, 12, 10), (3, 7, 1)]
        .map(|(id, a, b)| (nums::id.eq(id), nums::a.eq(a), nums::b.eq(b)));
    diesel::insert_into(nums::table).values(&rows[..]).execute(connection).expect("seed rows");
    sql_query(query)
        .load::<Row>(connection)
        .expect("run the query")
        .into_iter()
        .map(|row| row.r)
        .collect()
}

fn run(pg: &str) -> Vec<i64> {
    let statements = translate(&format!("{TABLE}{pg}"), &Pg2SqliteOptions::default());
    loaded(&mut SqliteConnection::establish(":memory:").expect("connect"), &statements)
}

#[test]
fn chained_xor_grows_linearly_and_answers() {
    let source =
        format!("SELECT {} AS r", (1..=12).map(|n| n.to_string()).collect::<Vec<_>>().join(" # "));
    let statements = translate(&source, &Pg2SqliteOptions::default());
    let emitted = statements.last().expect("a query");
    assert!(
        emitted.len() < 40 * source.len(),
        "{} bytes for {} source bytes",
        emitted.len(),
        source.len()
    );
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    let value = sql_query(emitted).get_result::<Row>(&mut connection).expect("run").r;
    assert_eq!(value, 12);
}

#[test]
fn chained_xor_over_columns_answers_per_row() {
    assert_eq!(run("SELECT a # b # id AS r FROM nums ORDER BY id;"), [7, 4, 5]);
}

#[test]
fn chained_xor_over_a_scalar_call_answers_per_row() {
    assert_eq!(run("SELECT abs(a - 20) # b # id AS r FROM nums ORDER BY id;"), [13, 0, 15]);
}

#[test]
fn queries_refuse_a_repeated_volatile_operand() {
    let message = refusal("SELECT random() # 7 AS r;");
    assert!(message.contains("reads"), "{message}");
}

#[test]
fn aggregate_operands_keep_their_grouping() {
    assert_eq!(run("SELECT sum(a) # 1 AS r FROM nums;"), [25]);
    assert_eq!(
        run("SELECT count(*) # 1 AS r FROM nums GROUP BY b > 2 HAVING count(*) # 1 > 0;"),
        [3]
    );
}

#[test]
fn window_operands_keep_their_frame() {
    assert_eq!(
        run("SELECT row_number() OVER (ORDER BY id) # 1 AS r FROM nums ORDER BY id;"),
        [0, 3, 2]
    );
}

fn refusal(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect_err("translation should be refused")
        .to_string()
}

#[test]
fn definitions_refuse_a_repeated_operand_that_repeats() {
    let message = refusal("CREATE TABLE t (a INT, b INT, c INT, CHECK (a # b # c > 0));");
    assert!(message.contains("exponentially"), "{message}");
}

#[test]
fn definitions_refuse_a_repeated_volatile_operand() {
    let message = refusal("CREATE TABLE t (a INT CHECK ((random() * 10)::int # 1 >= 0));");
    assert!(message.contains("reads"), "{message}");
}
