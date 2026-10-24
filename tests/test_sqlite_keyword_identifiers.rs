//! PostgreSQL lets a source name a table, column or alias with a word SQLite
//! reserves, such as `set`, `values` or `not`, so the emitted SQLite quotes it.
//!
//! Every expectation was read off PostgreSQL 18.6. The emitted statements are
//! executed as text because that text is the artifact under test.

use diesel::{prelude::*, sql_types::Integer};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

const FIXTURE: &str = "CREATE TABLE t (id int PRIMARY KEY, set int, values int);
     INSERT INTO t (id, set, values) VALUES (1, 2, 3);
     UPDATE t SET set = set + 10 WHERE values = 3;";

/// The rows the last emitted statement returns after every earlier one ran.
fn rows<T: QueryableByName<diesel::sqlite::Sqlite> + 'static>(pg: &str) -> Vec<T> {
    let statements = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
        .into_vec();
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    let (probe, setup) = statements.split_last().expect("an emitted query");
    for statement in setup {
        diesel::sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted setup failed: {statement}: {error}"));
    }
    diesel::sql_query(probe)
        .load::<T>(&mut connection)
        .unwrap_or_else(|error| panic!("emitted query failed: {probe}: {error}"))
}

#[derive(QueryableByName, Debug, PartialEq)]
struct Columns {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Integer, column_name = "set")]
    set_: i32,
    #[diesel(sql_type = Integer, column_name = "values")]
    values_: i32,
}

/// Columns named by SQLite keywords work in `INSERT`, `UPDATE`, `WHERE` and the
/// projection.
#[test]
fn keyword_named_columns_work_through_dml() {
    let read = rows::<Columns>(&format!("{FIXTURE}\nSELECT id, set, values FROM t ORDER BY id;"));
    assert_eq!(read, [Columns { id: 1, set_: 12, values_: 3 }]);
}

#[derive(QueryableByName, Debug, PartialEq)]
struct Aliased {
    #[diesel(sql_type = Integer, column_name = "not")]
    not_: i32,
    #[diesel(sql_type = Integer, column_name = "set")]
    set_: i32,
}

/// A bare keyword alias and a qualified keyword column keep the lowercase names
/// PostgreSQL gives them.
#[test]
fn keyword_aliases_keep_postgres_column_names() {
    let read = rows::<Aliased>(&format!("{FIXTURE}\nSELECT id NOT, s.SET FROM t AS s;"));
    assert_eq!(read, [Aliased { not_: 1, set_: 12 }]);
}

#[derive(QueryableByName, Debug, PartialEq)]
struct Single {
    #[diesel(sql_type = Integer)]
    x: i32,
}

/// A table named by a SQLite keyword is created, written and read.
#[test]
fn keyword_named_table_is_created_and_read() {
    let read =
        rows::<Single>("CREATE TABLE set (x int); INSERT INTO set VALUES (7); SELECT x FROM set;");
    assert_eq!(read, [Single { x: 7 }]);
}
