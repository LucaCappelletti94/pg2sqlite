//! Bound parameters at DML value positions must be converted to the column
//! representation the SQLite schema holds.
//!
//! A caller binds what PostgreSQL takes: a decimal for a `NUMERIC` column.
//! The emitted SQL performs the minor-unit conversion, so the caller never
//! needs to know that the column is stored as a scaled integer. Covered
//! positions: `INSERT VALUES`, `INSERT ... SELECT`, multi-row `VALUES`,
//! `UPDATE SET`, `ON CONFLICT DO UPDATE SET`, and the `INSTEAD OF
//! INSERT`/`UPDATE` triggers the RLS view path interposes.

mod helpers;

use diesel::{RunQueryDsl, prelude::*};
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    // Direct table: NUMERIC(10,2) translates to INTEGER minor units in SQLite.
    diesel::table! {
        ledger (id) {
            id -> Integer,
            amount -> BigInt,
        }
    }

    // Backing table that the RLS view writes through.
    // Read directly in tests so assertions see what the trigger stored.
    diesel::table! {
        pay_rls (id) {
            id -> Integer,
            amount -> BigInt,
        }
    }
}

use schema::{ledger, pay_rls};

/// Translates `pg`, applies every emitted statement but the last to a fresh
/// in-memory connection, and returns (connection, last_statement_string).
///
/// The last statement is the parameterized DML under test; everything before
/// it is DDL (or a non-parameterized seed INSERT) that must be applied with
/// `diesel::sql_query` because the emitted form includes `STRICT` tables,
/// `CHECK` constraints, and `CREATE TRIGGER` bodies that Diesel's typed DSL
/// cannot express.
fn setup_for_dml(pg: &str, opts: &Pg2SqliteOptions) -> (SqliteConnection, String) {
    let mut stmts =
        Pg2Sqlite::default().sql(pg).expect("parse").translate_to_sql(opts).expect("translate");
    let dml = stmts.pop().expect("translation must emit at least one statement");
    let mut conn = establish_connection();
    for stmt in &stmts {
        // Emitted DDL includes STRICT tables, CHECK constraints, and trigger
        // bodies that Diesel's typed DSL cannot express.
        diesel::sql_query(stmt.as_str())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("setup failed: {e}\n{stmt}"));
    }
    (conn, dml)
}

fn default_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
}

fn rls_opts() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit")
}

const LEDGER_DDL: &str =
    "CREATE TABLE ledger (id INTEGER PRIMARY KEY, amount NUMERIC(10,2) NOT NULL);";

/// `INSERT VALUES` with a `NUMERIC` parameter must scale the bound decimal to
/// minor units.
///
/// Red: the raw `?1` passes `1.5` (REAL) to a STRICT INTEGER column and the
/// execute fails with "cannot store REAL value in INTEGER column".  Green: the
/// emitted SQL wraps it in `CAST(ROUND(?1 * 100) AS INTEGER)`.
#[test]
fn insert_values_numeric_param_is_scaled() {
    let pg = format!("{LEDGER_DDL}INSERT INTO ledger (id, amount) VALUES (1, $1);");
    let (mut conn, insert_sql) = setup_for_dml(&pg, &default_opts());

    // diesel::sql_query is required: the test verifies that the translator's
    // emitted SQL wraps ?1 in a conversion expression. The typed DSL would
    // generate its own SQL, bypassing the translator output entirely.
    diesel::sql_query(&insert_sql)
        .bind::<diesel::sql_types::Double, _>(1.5_f64)
        .execute(&mut conn)
        .expect("binding 1.5 for NUMERIC(10,2) must succeed");

    let stored: i64 = ledger::table
        .select(ledger::amount)
        .filter(ledger::id.eq(1_i32))
        .first(&mut conn)
        .expect("row must be present");
    assert_eq!(stored, 150, "1.5 must be stored as 150 minor units");
}

/// A parameter in the second row of a multi-row `VALUES` must be scaled.
///
/// `for_each_insert_position` visits every row; only the second row carries
/// the parameter here, verifying the loop covers all rows.
#[test]
fn insert_values_multi_row_later_param_is_scaled() {
    let pg = format!("{LEDGER_DDL}INSERT INTO ledger (id, amount) VALUES (1, 1.00), (2, $1);");
    let (mut conn, insert_sql) = setup_for_dml(&pg, &default_opts());

    diesel::sql_query(&insert_sql)
        .bind::<diesel::sql_types::Double, _>(2.5_f64)
        .execute(&mut conn)
        .expect("binding 2.5 in the second row for NUMERIC(10,2) must succeed");

    let stored_1: i64 = ledger::table
        .select(ledger::amount)
        .filter(ledger::id.eq(1_i32))
        .first(&mut conn)
        .expect("first row must exist");
    assert_eq!(stored_1, 100, "literal 1.00 must be stored as 100 minor units");

    let stored_2: i64 = ledger::table
        .select(ledger::amount)
        .filter(ledger::id.eq(2_i32))
        .first(&mut conn)
        .expect("second row must exist");
    assert_eq!(stored_2, 250, "parameter 2.5 must be stored as 250 minor units");
}

/// An `INSERT ... SELECT` whose projection carries a `NUMERIC` parameter must
/// scale it.
///
/// `for_each_insert_position` handles the `Select` arm of `SetExpr`; this
/// exercises that branch.
#[test]
fn insert_select_numeric_param_is_scaled() {
    let pg = format!("{LEDGER_DDL}INSERT INTO ledger (id, amount) SELECT 1, $1;");
    let (mut conn, insert_sql) = setup_for_dml(&pg, &default_opts());

    diesel::sql_query(&insert_sql)
        .bind::<diesel::sql_types::Double, _>(3.75_f64)
        .execute(&mut conn)
        .expect("binding 3.75 in INSERT SELECT for NUMERIC(10,2) must succeed");

    let stored: i64 = ledger::table
        .select(ledger::amount)
        .filter(ledger::id.eq(1_i32))
        .first(&mut conn)
        .expect("row must exist");
    assert_eq!(stored, 375, "3.75 must be stored as 375 minor units");
}

/// `UPDATE ... SET` with a `NUMERIC` parameter must scale the bound decimal.
///
/// The update path goes through `ColumnRewrites::finish_assignment`; the
/// same extension that fixes INSERT covers it once `finish_value` handles
/// `Placeholder`.
#[test]
fn update_set_numeric_param_is_scaled() {
    let pg = format!(
        "{LEDGER_DDL}\
         INSERT INTO ledger (id, amount) VALUES (1, 1.00);\
         UPDATE ledger SET amount = $1 WHERE id = 1;"
    );
    let (mut conn, update_sql) = setup_for_dml(&pg, &default_opts());

    diesel::sql_query(&update_sql)
        .bind::<diesel::sql_types::Double, _>(2.5_f64)
        .execute(&mut conn)
        .expect("binding 2.5 to UPDATE SET NUMERIC(10,2) must succeed");

    let stored: i64 = ledger::table
        .select(ledger::amount)
        .filter(ledger::id.eq(1_i32))
        .first(&mut conn)
        .expect("row must exist after update");
    assert_eq!(stored, 250, "2.5 must update to 250 minor units");
}

/// `ON CONFLICT ... DO UPDATE SET` with a `NUMERIC` parameter must scale it.
///
/// The upsert path goes through `translate_on_conflict_do_update` →
/// `ColumnRewrites::finish_assignment`; covered by the same `finish_value`
/// extension.
#[test]
fn upsert_do_update_numeric_param_is_scaled() {
    let pg = format!(
        "{LEDGER_DDL}\
         INSERT INTO ledger (id, amount) VALUES (1, 1.00);\
         INSERT INTO ledger (id, amount) VALUES (1, 1.00) \
             ON CONFLICT (id) DO UPDATE SET amount = $1;"
    );
    let (mut conn, upsert_sql) = setup_for_dml(&pg, &default_opts());

    diesel::sql_query(&upsert_sql)
        .bind::<diesel::sql_types::Double, _>(4.99_f64)
        .execute(&mut conn)
        .expect("binding 4.99 in DO UPDATE SET NUMERIC(10,2) must succeed");

    let stored: i64 = ledger::table
        .select(ledger::amount)
        .filter(ledger::id.eq(1_i32))
        .first(&mut conn)
        .expect("row must exist after upsert");
    assert_eq!(stored, 499, "4.99 must upsert to 499 minor units");
}

const PAY_DDL: &str = "
    CREATE TABLE pay (id INTEGER PRIMARY KEY, amount NUMERIC(10,2) NOT NULL);
    ALTER TABLE pay ENABLE ROW LEVEL SECURITY;
    CREATE POLICY pay_all ON pay FOR ALL USING (true) WITH CHECK (true);
";

/// A parameter bound through an RLS view's `INSTEAD OF INSERT` trigger must
/// be converted the same way a direct insert is.
///
/// The INSERT statement-level conversion wraps the placeholder before it
/// reaches SQLite; the trigger receives `NEW.amount` already as a scaled
/// integer and forwards it to the STRICT INTEGER backing column.
#[test]
fn rls_insert_numeric_param_is_scaled() {
    let pg = format!("{PAY_DDL}INSERT INTO pay (id, amount) VALUES (1, $1);");
    let (mut conn, insert_sql) = setup_for_dml(&pg, &rls_opts());

    diesel::sql_query(&insert_sql)
        .bind::<diesel::sql_types::Double, _>(1.5_f64)
        .execute(&mut conn)
        .expect("binding 1.5 through RLS view for NUMERIC(10,2) must succeed");

    let stored: i64 = pay_rls::table
        .select(pay_rls::amount)
        .filter(pay_rls::id.eq(1_i32))
        .first(&mut conn)
        .expect("row must exist in backing table");
    assert_eq!(stored, 150, "1.5 through RLS view must be stored as 150 minor units");
}

/// A parameter bound through an RLS view's `INSTEAD OF UPDATE` trigger must
/// be converted the same way a direct update is.
///
/// The UPDATE statement-level conversion scales `?1` before it reaches the
/// view; the trigger's `NEW.amount` is already a scaled integer.
#[test]
fn rls_update_numeric_param_is_scaled() {
    let pg = format!(
        "{PAY_DDL}\
         INSERT INTO pay (id, amount) VALUES (1, 1.00);\
         UPDATE pay SET amount = $1 WHERE id = 1;"
    );
    let (mut conn, update_sql) = setup_for_dml(&pg, &rls_opts());

    diesel::sql_query(&update_sql)
        .bind::<diesel::sql_types::Double, _>(3.25_f64)
        .execute(&mut conn)
        .expect("binding 3.25 through RLS view UPDATE for NUMERIC(10,2) must succeed");

    let stored: i64 = pay_rls::table
        .select(pay_rls::amount)
        .filter(pay_rls::id.eq(1_i32))
        .first(&mut conn)
        .expect("row must exist in backing table after update");
    assert_eq!(stored, 325, "3.25 through RLS view must update to 325 minor units");
}
