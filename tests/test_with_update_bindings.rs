//! Runtime regressions for `WITH`-wrapped `UPDATE` translation.

mod helpers;

use diesel::{prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::Pg2SqliteOptions;
use sqlparser::ast::Statement;

mod schema {
    diesel::table! {
        w1_dst (id) {
            id -> Integer,
            v -> Integer,
        }
    }

    diesel::table! {
        u1_dst (id) {
            id -> Integer,
            v -> Integer,
        }
    }

    diesel::table! {
        b1_dst (id) {
            id -> Integer,
            v -> Integer,
        }
    }

    diesel::table! {
        c2_dst (id) {
            id -> Integer,
            v -> Integer,
        }
    }
}

use schema::{b1_dst, c2_dst, u1_dst, w1_dst};

/// `true` for a bare `UPDATE` statement or a `WITH`-wrapped one emitted as a
/// query.
fn is_update(statement: &Statement) -> bool {
    match statement {
        Statement::Update(_) => true,
        Statement::Query(query) => {
            matches!(query.body.as_ref(), sqlparser::ast::SetExpr::Update(_))
        }
        _ => false,
    }
}

/// Translate `pg`, run every emitted statement but the `UPDATE` and return
/// the `UPDATE`.
fn prepared_update(pg: &str) -> (SqliteConnection, String) {
    let statements = helpers::translate_statements(pg, &Pg2SqliteOptions::default())
        .unwrap_or_else(|error| panic!("translation failed: {error}\n{pg}"));
    let mut conn = helpers::establish_connection();
    let mut update = None;
    for statement in &statements {
        let rendered = statement.to_string();
        if is_update(statement) {
            update = Some(rendered);
        } else {
            diesel::sql_query(rendered.as_str())
                .execute(&mut conn)
                .unwrap_or_else(|error| panic!("setup DDL failed {error}\n{rendered}"));
        }
    }
    (conn, update.expect("an `UPDATE` must be emitted"))
}

/// Run the emitted `UPDATE`, whose `WITH`-wrapped `FROM` form the query DSL
/// cannot express.
fn execute_update(conn: &mut SqliteConnection, update: &str) {
    diesel::sql_query(update)
        .execute(conn)
        .unwrap_or_else(|error| panic!("translated update failed {error}\n{update}"));
}

#[test]
fn with_update_target_column_arithmetic_uses_physical_target() {
    let pg = "CREATE TABLE w1_dst(id INT PRIMARY KEY, v INT);
              INSERT INTO w1_dst VALUES(1, 10);
              WITH w1_dst AS (SELECT 99 AS v)
              UPDATE w1_dst SET v = v + 1;";
    let (mut conn, update) = prepared_update(pg);
    execute_update(&mut conn, &update);
    let rows: Vec<(i32, i32)> = w1_dst::table
        .select((w1_dst::id, w1_dst::v))
        .order(w1_dst::id.asc())
        .load(&mut conn)
        .expect("final state must load");
    assert_eq!(rows, vec![(1, 11)]);
}

#[test]
fn with_update_bare_target_alias_and_cte_auxiliary_read() {
    let pg = "CREATE TABLE u1_b(v INT);
              CREATE TABLE u1_src(v INT);
              CREATE TABLE u1_dst(id INT, v INT);
              INSERT INTO u1_b VALUES(1), (2);
              INSERT INTO u1_src VALUES(10), (20);
              INSERT INTO u1_dst VALUES(1, 0), (2, 0);
              WITH a AS (SELECT v FROM u1_b),
                   u1_b AS (SELECT v FROM u1_src)
              UPDATE u1_dst d SET v = a.v FROM a WHERE d.id = a.v;";
    let (mut conn, update) = prepared_update(pg);
    execute_update(&mut conn, &update);
    let rows: Vec<(i32, i32)> = u1_dst::table
        .select((u1_dst::id, u1_dst::v))
        .order(u1_dst::id.asc())
        .load(&mut conn)
        .expect("final state must load");
    assert_eq!(rows, vec![(1, 1), (2, 2)]);
}

#[test]
fn with_update_assignment_reads_enclosing_cte() {
    let pg = "
        CREATE TABLE b1_dst (id INT PRIMARY KEY, v INT);
        INSERT INTO b1_dst VALUES (1, 0);
        WITH b1 AS (SELECT 5 AS x)
        UPDATE b1_dst SET v = (SELECT x FROM b1) WHERE b1_dst.id = 1;
    ";
    let (mut conn, update) = prepared_update(pg);
    execute_update(&mut conn, &update);
    let rows: Vec<(i32, i32)> = b1_dst::table
        .select((b1_dst::id, b1_dst::v))
        .order(b1_dst::id.asc())
        .load(&mut conn)
        .expect("final state must load");
    assert_eq!(rows, vec![(1, 5)]);
}

#[test]
fn ordinary_update_control() {
    let pg = "
        CREATE TABLE c2_dst (id INT PRIMARY KEY, v INT);
        INSERT INTO c2_dst VALUES (1, 5);
        UPDATE c2_dst SET v = v * 2 WHERE c2_dst.id = 1;
    ";
    let (mut conn, update) = prepared_update(pg);
    execute_update(&mut conn, &update);
    let rows: Vec<(i32, i32)> = c2_dst::table
        .select((c2_dst::id, c2_dst::v))
        .order(c2_dst::id.asc())
        .load(&mut conn)
        .expect("final state must load");
    assert_eq!(rows, vec![(1, 10)]);
}
