//! Tests for forward UPDATE translation in
//! `src/impls/translator_impls/update.rs`.

mod helpers;
#[path = "helpers/translate.rs"]
mod translate_helpers;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use rusqlite::Connection;
use sqlparser::{
    ast::{
        BinaryOperator, Expr, Ident, Join, JoinConstraint, JoinOperator, ObjectName,
        ObjectNamePart, Statement, TableFactor,
    },
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

#[test]
fn forward_update_basic() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        UPDATE users SET name = 'Bob' WHERE id = 1;
    ";
    let output = translate(sql);
    assert!(output.contains("UPDATE"), "Expected UPDATE in output: {output}");
    assert!(output.contains("name = 'Bob'"), "Expected updated value in output: {output}");
    execute_all(&output);
}

#[test]
fn forward_update_from_is_preserved() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, team_id INT, name TEXT);
        CREATE TABLE teams (id INT PRIMARY KEY, name TEXT);
        UPDATE users SET name = teams.name
        FROM teams
        WHERE users.team_id = teams.id;
    ";
    let output = translate(sql);
    assert!(output.contains("UPDATE"), "Expected UPDATE in output: {output}");
    assert!(output.contains("FROM teams"), "Expected FROM clause in output: {output}");
    execute_all(&output);
}

#[test]
fn forward_update_translates_assignment_expressions() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, updated_at TEXT);
        UPDATE users SET updated_at = now() WHERE id = 1;
    ";
    let output = translate(sql);
    assert!(
        output.contains("strftime('%Y-%m-%d %H:%M:%f000+00:00', 'now')"),
        "Expected now() to translate to strftime('%Y-%m-%d %H:%M:%f000+00:00', 'now'): {output}"
    );
    execute_all(&output);
}

#[test]
fn update_with_joined_target_table_is_rejected() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, team_id INT, name TEXT);
        CREATE TABLE teams (id INT PRIMARY KEY, name TEXT);
    ";

    let mut update_stmt =
        Parser::parse_sql(&PostgreSqlDialect {}, "UPDATE users SET name = 'x' WHERE id = 1;")
            .unwrap()
            .pop()
            .unwrap();

    let Statement::Update(update) = &mut update_stmt else {
        panic!("Expected UPDATE statement");
    };

    update.table.joins.push(Join {
        relation: TableFactor::Table {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("teams"))]),
            alias: None,
            args: None,
            with_hints: vec![],
            version: None,
            partitions: vec![],
            json_path: None,
            sample: None,
            index_hints: vec![],
            with_ordinality: false,
        },
        global: false,
        join_operator: JoinOperator::Inner(JoinConstraint::On(Expr::BinaryOp {
            left: Box::new(Expr::CompoundIdentifier(vec![
                Ident::new("users"),
                Ident::new("team_id"),
            ])),
            op: BinaryOperator::Eq,
            right: Box::new(Expr::CompoundIdentifier(vec![Ident::new("teams"), Ident::new("id")])),
        })),
    });

    let result = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .statement(update_stmt)
        .translate(&Pg2SqliteOptions::default());

    assert!(result.is_err(), "Expected unsupported UPDATE target join error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("UPDATE with joins on the target table"),
        "Expected target-table-join error, got: {err}"
    );
}

#[test]
fn forward_update_limit_translates_expressions() {
    let schema_sql = "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);";
    let mut update_stmt =
        Parser::parse_sql(&PostgreSqlDialect {}, "UPDATE users SET name = 'x' WHERE id = 1;")
            .unwrap()
            .remove(0);

    let Statement::Update(update) = &mut update_stmt else {
        panic!("Expected UPDATE statement");
    };
    update.limit = Some(parse_expr("NOW()"));

    let stmts = Pg2Sqlite::default()
        .sql(schema_sql)
        .unwrap()
        .statement(update_stmt)
        .translate(&Pg2SqliteOptions::default())
        .unwrap();

    let output = stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");

    assert!(
        output.contains("LIMIT")
            && output.contains("strftime('%Y-%m-%d %H:%M:%f000+00:00', 'now')"),
        "Expected translated LIMIT expression in UPDATE: {output}"
    );

    // Execute DDL statements. The UPDATE uses LIMIT which requires
    // SQLITE_ENABLE_UPDATE_DELETE_LIMIT at compile time, so only the DDL
    // (non-Update) statements are run here to prove SQLite accepts the schema.
    let conn = Connection::open_in_memory().unwrap();
    for s in stmts.iter().filter(|s| !matches!(s, Statement::Update(_))) {
        conn.execute_batch(&format!("{s};")).unwrap();
    }
}

/// Target aliases preserve stored and returned values.
mod update_alias {
    use diesel::{prelude::*, sqlite::SqliteConnection};
    use pg2sqlite::prelude::Pg2SqliteOptions;
    use sqlparser::ast::Statement;

    use super::helpers;

    diesel::table! {
        /// Update target rows.
        t (id) {
            /// A stable row identifier.
            id -> Integer,
            /// An assigned value.
            a -> Integer,
            /// An independent source value.
            b -> Integer,
        }
    }
    diesel::table! {
        /// Auxiliary update inputs.
        f (id) {
            /// A target row identifier.
            id -> Integer,
            /// An independent input value.
            x -> Integer,
        }
    }

    #[derive(QueryableByName, Debug, PartialEq, Eq)]
    struct ReturningRow {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        id: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        a: i32,
    }

    /// Applies translated DDL and retains the update for execution.
    fn prepared_update(pg: &str) -> (SqliteConnection, String) {
        let statements = helpers::translate_statements(pg, &Pg2SqliteOptions::default())
            .expect("translation should succeed");
        let mut conn = helpers::establish_connection();
        let mut update = None;
        for statement in &statements {
            let rendered = statement.to_string();
            match statement {
                Statement::Update(_) => update = Some(rendered),
                _ => {
                    // Translated DDL is runtime syntax under test.
                    diesel::sql_query(rendered.as_str())
                        .execute(&mut conn)
                        .unwrap_or_else(|error| panic!("setup DDL failed {error}\n{rendered}"));
                }
            }
        }
        (conn, update.expect("an UPDATE must be emitted"))
    }

    fn insert_t(conn: &mut SqliteConnection) {
        diesel::insert_into(t::table)
            .values([
                (t::id.eq(1), t::a.eq(10), t::b.eq(100)),
                (t::id.eq(2), t::a.eq(20), t::b.eq(200)),
                (t::id.eq(3), t::a.eq(30), t::b.eq(300)),
            ])
            .execute(conn)
            .expect("fixture rows must insert");
    }

    fn read_t(conn: &mut SqliteConnection) -> Vec<(i32, i32, i32)> {
        t::table
            .select((t::id, t::a, t::b))
            .order(t::id.asc())
            .load::<(i32, i32, i32)>(conn)
            .expect("final state must load")
    }

    /// Runs the translated UPDATE and asserts the stored rows.
    fn run_and_assert_t(pg: &str, expected: &[(i32, i32, i32)]) {
        let (mut conn, update) = prepared_update(pg);
        insert_t(&mut conn);
        // Translated update is runtime syntax under test.
        diesel::sql_query(update.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|error| panic!("translated update failed {error}\n{update}"));
        assert_eq!(read_t(&mut conn), expected);
    }

    #[test]
    fn update_alias_without_as_executes_in_sqlite() {
        run_and_assert_t(
            "CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);\n\
             UPDATE t se SET a = a + 1 WHERE id = 2;",
            &[(1, 10, 100), (2, 21, 200), (3, 30, 300)],
        );
    }

    #[test]
    fn update_quoted_alias_without_as_executes_in_sqlite() {
        run_and_assert_t(
            "CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);\n\
             UPDATE t \"Se\" SET a = a + 1 WHERE \"Se\".id = 2;",
            &[(1, 10, 100), (2, 21, 200), (3, 30, 300)],
        );
    }

    #[test]
    fn update_alias_with_as_keeps_qualified_predicate() {
        run_and_assert_t(
            "CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);\n\
             UPDATE t AS se SET a = a + 1 WHERE se.id = 3;",
            &[(1, 10, 100), (2, 20, 200), (3, 31, 300)],
        );
    }

    #[test]
    fn update_alias_with_as_keeps_qualified_assignment() {
        run_and_assert_t(
            "CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);\n\
             UPDATE t AS se SET a = se.b + 5 WHERE se.id = 1;",
            &[(1, 105, 100), (2, 20, 200), (3, 30, 300)],
        );
    }

    #[test]
    fn update_quoted_alias_with_as_executes_in_sqlite() {
        run_and_assert_t(
            "CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);\n\
             UPDATE t AS \"Se\" SET a = a + 1 WHERE \"Se\".id = 2;",
            &[(1, 10, 100), (2, 21, 200), (3, 30, 300)],
        );
    }

    #[test]
    fn update_from_with_target_alias_maps_columns() {
        let (mut conn, update) = prepared_update(
            "CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);\n\
             CREATE TABLE f (id INT PRIMARY KEY, x INT);\n\
             UPDATE t AS se SET a = f.x FROM f WHERE se.id = f.id;",
        );
        insert_t(&mut conn);
        diesel::insert_into(f::table)
            .values([(f::id.eq(1), f::x.eq(77)), (f::id.eq(2), f::x.eq(88))])
            .execute(&mut conn)
            .expect("f fixture rows must insert");
        // Translated update and its joined inputs are runtime syntax under
        // test.
        diesel::sql_query(update.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|error| panic!("translated update failed {error}\n{update}"));
        assert_eq!(read_t(&mut conn), vec![(1, 77, 100), (2, 88, 200), (3, 30, 300)]);
    }

    #[test]
    fn update_alias_with_as_returning_rewrites_to_target() {
        let (mut conn, update) = prepared_update(
            "CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT);\n\
             UPDATE t AS se SET a = 99 WHERE se.id = 2 RETURNING se.id, se.a;",
        );
        insert_t(&mut conn);
        // Translated update and returning projection are runtime syntax under
        // test.
        let returned: Vec<ReturningRow> = diesel::sql_query(update.as_str())
            .load(&mut conn)
            .unwrap_or_else(|error| panic!("translated update failed {error}\n{update}"));
        assert_eq!(returned, vec![ReturningRow { id: 2, a: 99 }]);
        assert_eq!(read_t(&mut conn), vec![(1, 10, 100), (2, 99, 200), (3, 30, 300)]);
    }
}
