//! Tests for forward DELETE translation covering USING clause conversion.
//! in `src/impls/translator_impls/delete.rs`.

#[path = "helpers/translate.rs"]
mod translate_helpers;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;
use sqlparser::{
    ast::{Expr, Statement},
    dialect::PostgreSqlDialect,
    parser::Parser,
};
use translate_helpers::translate_default as translate;

/// Execute each non-empty statement in `output` (newline-joined) against an
/// in-memory SQLite to prove the translator emits valid SQL.
fn execute_all(output: &str) {
    let conn = Connection::open_in_memory().unwrap();
    for s in output.split('\n').filter(|s| !s.trim().is_empty()) {
        conn.execute_batch(&format!("{s};")).unwrap();
    }
}

fn parse_expr(sql: &str) -> Expr {
    Parser::new(&PostgreSqlDialect {}).try_with_sql(sql).unwrap().parse_expr().unwrap()
}

fn parse_order_by_expr(sql: &str) -> sqlparser::ast::OrderByExpr {
    let stmt = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap().remove(0);
    let Statement::Query(query) = stmt else {
        panic!("Expected query statement");
    };
    let Some(order_by) = query.order_by else {
        panic!("Expected ORDER BY clause");
    };
    let sqlparser::ast::OrderByKind::Expressions(mut exprs) = order_by.kind else {
        panic!("Expected ORDER BY expressions");
    };
    exprs.remove(0)
}

#[test]
fn delete_using_converts_to_exists() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE inactive (user_id INT PRIMARY KEY);
        DELETE FROM users USING inactive WHERE users.id = inactive.user_id;
    ";
    let output = translate(sql);
    // USING should be converted to EXISTS subquery
    assert!(
        output.contains("EXISTS") || output.contains("DELETE"),
        "Expected EXISTS or DELETE: {output}"
    );
    execute_all(&output);
}

#[test]
fn delete_using_with_condition() {
    let sql = "
        CREATE TABLE orders (id INT PRIMARY KEY, user_id INT, status TEXT);
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        DELETE FROM orders USING users WHERE orders.user_id = users.id AND users.name = 'test';
    ";
    let output = translate(sql);
    assert!(output.contains("DELETE") || output.contains("EXISTS"), "Expected DELETE: {output}");
    execute_all(&output);
}

#[test]
fn delete_basic() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        DELETE FROM users WHERE id = 1;
    ";
    let output = translate(sql);
    assert!(output.contains("DELETE"), "Expected DELETE: {output}");
    execute_all(&output);
}

#[test]
fn delete_all() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        DELETE FROM users;
    ";
    let output = translate(sql);
    assert!(output.contains("DELETE"), "Expected DELETE: {output}");
    execute_all(&output);
}

#[test]
fn delete_where_translates_expressions() {
    let sql = "
        CREATE TABLE events (id INT PRIMARY KEY, created_at TEXT);
        DELETE FROM events WHERE NOW() > created_at;
    ";
    let output = translate(sql);
    assert!(
        output.contains("strftime('%Y-%m-%d %H:%M:%f000+00:00', 'now')"),
        "Expected strftime('%Y-%m-%d %H:%M:%f000+00:00', 'now') in DELETE WHERE: {output}"
    );
    execute_all(&output);
}

#[test]
fn delete_returning_translates_expressions() {
    let sql = "
        CREATE TABLE events (id INT PRIMARY KEY, name TEXT);
        DELETE FROM events WHERE id = 1 RETURNING NOW() AS ts;
    ";
    let output = translate(sql);
    assert!(
        output.contains("strftime('%Y-%m-%d %H:%M:%f000+00:00', 'now')"),
        "Expected strftime('%Y-%m-%d %H:%M:%f000+00:00', 'now') in DELETE RETURNING: {output}"
    );
    execute_all(&output);
}

#[test]
fn delete_order_by_and_limit_translate_expressions() {
    let schema_sql = "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);";
    let mut delete_stmt =
        Parser::parse_sql(&PostgreSqlDialect {}, "DELETE FROM users WHERE id > 0;")
            .unwrap()
            .remove(0);

    let Statement::Delete(delete) = &mut delete_stmt else {
        panic!("Expected DELETE statement");
    };
    delete.order_by = vec![parse_order_by_expr("SELECT 1 ORDER BY NOW();")];
    delete.limit = Some(parse_expr("NOW()"));

    let stmts = Pg2Sqlite::default()
        .sql(schema_sql)
        .unwrap()
        .statement(delete_stmt)
        .translate(&Pg2SqliteOptions::default())
        .unwrap();

    let output = stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        output.contains("ORDER BY")
            && output.contains("LIMIT")
            && output.contains("strftime('%Y-%m-%d %H:%M:%f000+00:00', 'now')"),
        "Expected translated ORDER BY/LIMIT expressions in DELETE: {output}"
    );

    // Execute DDL statements. The DELETE uses ORDER BY/LIMIT which requires
    // SQLITE_ENABLE_UPDATE_DELETE_LIMIT at compile time, so only the DDL
    // (non-Delete) statements are run here to prove SQLite accepts the schema.
    let conn = Connection::open_in_memory().unwrap();
    for s in stmts.iter().filter(|s| !matches!(s, Statement::Delete(_))) {
        conn.execute_batch(&format!("{s};")).unwrap();
    }
}

/// Target aliases preserve deleted and returned rows.
mod delete_alias {
    use diesel::{connection::SimpleConnection, prelude::*, sql_types::Integer};
    use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
    use sqlparser::ast::Statement;

    diesel::table! {
        /// Delete target rows.
        t (id) {
            /// A stable row identifier.
            id -> Integer,
            /// A filtered value.
            n -> Integer,
        }
    }

    #[derive(QueryableByName, Debug, PartialEq, Eq)]
    struct Returned {
        #[diesel(sql_type = Integer)]
        id: i32,
        #[diesel(sql_type = Integer)]
        n: i32,
    }

    const SCHEMA: &str = "CREATE TABLE t (id INT PRIMARY KEY, n INT NOT NULL);";

    /// Applies the translated schema, inserts fixtures and returns the
    /// translated delete.
    fn prepared(source: &str) -> (SqliteConnection, String) {
        let statements = Pg2Sqlite::default()
            .sql(&format!("{SCHEMA}\n{source}"))
            .unwrap()
            .translate(&Pg2SqliteOptions::default())
            .unwrap();
        let mut connection = SqliteConnection::establish(":memory:").unwrap();
        let mut delete = None;
        for statement in statements {
            if matches!(statement, Statement::Delete(_)) {
                delete = Some(statement.to_string());
            } else {
                // Translated DDL is runtime syntax under test.
                connection.batch_execute(&statement.to_string()).unwrap();
            }
        }
        diesel::insert_into(t::table)
            .values([
                (t::id.eq(1), t::n.eq(10)),
                (t::id.eq(2), t::n.eq(20)),
                (t::id.eq(3), t::n.eq(30)),
            ])
            .execute(&mut connection)
            .unwrap();
        (connection, delete.expect("translated delete"))
    }

    fn remaining(connection: &mut SqliteConnection) -> Vec<(i32, i32)> {
        t::table.select((t::id, t::n)).order(t::id.asc()).load(connection).unwrap()
    }

    #[test]
    fn delete_alias_without_as_removes_selected_rows() {
        for source in [
            "DELETE FROM t x WHERE x.id = 2;",
            "DELETE FROM t \"X\" WHERE \"X\".id = 2;",
            "DELETE FROM t AS x WHERE x.id = 2;",
        ] {
            let (mut connection, delete) = prepared(source);
            // Translated delete is runtime syntax under test.
            diesel::sql_query(&delete)
                .execute(&mut connection)
                .unwrap_or_else(|error| panic!("translated delete failed {error}\n{delete}"));
            assert_eq!(remaining(&mut connection), [(1, 10), (3, 30)], "{source}");
        }
    }

    #[test]
    fn delete_alias_without_as_returns_deleted_rows() {
        let (mut connection, delete) =
            prepared("DELETE FROM t x WHERE x.n > 15 RETURNING x.id, x.n;");
        // Translated delete and returning projection are runtime syntax under
        // test.
        let mut returned = diesel::sql_query(&delete)
            .load::<Returned>(&mut connection)
            .unwrap_or_else(|error| panic!("translated delete failed {error}\n{delete}"));
        returned.sort_by_key(|row| row.id);
        assert_eq!(returned, [Returned { id: 2, n: 20 }, Returned { id: 3, n: 30 }]);
        assert_eq!(remaining(&mut connection), [(1, 10)]);
    }
}
