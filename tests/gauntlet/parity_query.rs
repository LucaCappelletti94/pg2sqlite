//! Query operand pagination and parameter parity, with translated SQL run raw
//! as the output under test.

use diesel::{
    connection::SimpleConnection,
    prelude::*,
    sql_types::{BigInt, Integer},
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlparser::ast::Statement;

use crate::{helpers::establish_connection, postgres_harness};

diesel::table! {
    nums (n) {
        n -> Integer,
    }
}

const SCHEMA: &str = "CREATE TABLE nums (n INTEGER NOT NULL);";

#[derive(QueryableByName)]
struct Number {
    #[diesel(sql_type = Integer)]
    n: i32,
}

#[test]
fn diesel_compound_queries_preserve_pagination_and_bind_identity() {
    let mut postgres = postgres_harness::fresh_database();
    let mut sqlite = establish_connection();
    postgres.batch_execute(SCHEMA).unwrap();
    sqlite.batch_execute(SCHEMA).unwrap();
    let rows = [1, 2, 3, 4, 5].map(|n| nums::n.eq(n));
    diesel::insert_into(nums::table).values(&rows).execute(&mut postgres).unwrap();
    diesel::insert_into(nums::table).values(&rows).execute(&mut sqlite).unwrap();

    let query = || {
        nums::table
            .select(nums::n)
            .order(nums::n.asc())
            .limit(3)
            .union_all(nums::table.select(nums::n).order(nums::n.desc()).limit(2).offset(1))
    };
    let source_sql = diesel::debug_query::<diesel::pg::Pg, _>(&query()).to_string();
    let mut source_rows = query().load::<i32>(&mut postgres).unwrap();
    source_rows.sort_unstable();
    assert_eq!(source_rows, [1, 2, 3, 3, 4]);
    let mut counterpart_rows = query().load::<i32>(&mut sqlite).unwrap();
    counterpart_rows.sort_unstable();
    assert_eq!(counterpart_rows, source_rows);

    let schema = Pg2Sqlite::default().sql(SCHEMA).unwrap().build_schema().unwrap();
    let translated = Pg2Sqlite::default()
        .sql(&source_sql)
        .unwrap()
        .translate_with_schema(&schema, &Pg2SqliteOptions::default())
        .unwrap()
        .into_iter()
        .find_map(|statement| {
            match statement {
                Statement::Query(query) => Some(query.to_string()),
                _ => None,
            }
        })
        .expect("translated query");
    let mut actual = diesel::sql_query(&translated)
        .bind::<BigInt, _>(3_i64)
        .bind::<BigInt, _>(2_i64)
        .bind::<BigInt, _>(1_i64)
        .load::<Number>(&mut sqlite)
        .unwrap()
        .into_iter()
        .map(|row| row.n)
        .collect::<Vec<_>>();
    actual.sort_unstable();
    assert_eq!(actual, source_rows);

    let reverse = Pg2Sqlite::default()
        .reverse_sql(&translated, &schema, &Pg2SqliteOptions::default())
        .unwrap()
        .into_iter()
        .find_map(|statement| {
            match statement {
                Statement::Query(query) => Some(query.to_string()),
                _ => None,
            }
        })
        .expect("reverse query");
    let mut reverse_rows = diesel::sql_query(&reverse)
        .bind::<BigInt, _>(3_i64)
        .bind::<BigInt, _>(2_i64)
        .bind::<BigInt, _>(1_i64)
        .load::<Number>(&mut postgres)
        .unwrap()
        .into_iter()
        .map(|row| row.n)
        .collect::<Vec<_>>();
    reverse_rows.sort_unstable();
    assert_eq!(reverse_rows, source_rows);
}
