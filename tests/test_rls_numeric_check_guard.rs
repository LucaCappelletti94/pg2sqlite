//! Guard substitution for a NUMERIC default must not scale the already-stored
//! minor-unit value a second time against the WITH CHECK predicate.

mod helpers;

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        ledger2 (id) {
            id -> Integer,
            amount -> BigInt,
            owner -> Text,
        }
    }

    diesel::table! {
        ledger2_rls (id) {
            id -> Integer,
            amount -> BigInt,
            owner -> Text,
        }
    }
}

use schema::{ledger2, ledger2_rls};

const TABLE_SQL: &str = "
    CREATE TABLE ledger2 (
        id INT PRIMARY KEY,
        amount NUMERIC(10,2) NOT NULL DEFAULT 1.50,
        owner TEXT NOT NULL
    );
    ALTER TABLE ledger2 ENABLE ROW LEVEL SECURITY;
";

fn apply(policy_sql: &str) -> SqliteConnection {
    let opts = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let sql = format!("{TABLE_SQL}{policy_sql}");
    // Emitted DDL cannot be expressed with the typed DSL.
    let stmts = Pg2Sqlite::default().sql(&sql).expect("parse").translate(&opts).expect("translate");
    let mut conn = helpers::establish_connection();
    for stmt in &stmts {
        diesel::sql_query(stmt.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("DDL failed: {e}\n{stmt}"));
    }
    conn
}

/// `DEFAULT 1.50` (stored as 150) satisfies `amount < 5.00`; insert must land.
#[test]
fn numeric_default_satisfies_check_policy() {
    let mut conn =
        apply("CREATE POLICY ledger2_ins ON ledger2 FOR INSERT WITH CHECK (amount < 5.00);");

    diesel::insert_into(ledger2::table)
        .values((ledger2::id.eq(1), ledger2::owner.eq("alice")))
        .execute(&mut conn)
        .expect("default 1.50 < 5.00, insert must succeed");

    let stored: i64 = ledger2_rls::table
        .filter(ledger2_rls::id.eq(1))
        .select(ledger2_rls::amount)
        .first(&mut conn)
        .expect("row must exist");

    assert_eq!(stored, 150, "amount must be 150 minor units");
}

/// `DEFAULT 1.50` violates `amount < 1.00`; insert must be refused.
#[test]
fn numeric_default_violates_check_policy() {
    let mut conn =
        apply("CREATE POLICY ledger2_ins ON ledger2 FOR INSERT WITH CHECK (amount < 1.00);");

    let err = diesel::insert_into(ledger2::table)
        .values((ledger2::id.eq(2), ledger2::owner.eq("alice")))
        .execute(&mut conn)
        .expect_err("default 1.50 >= 1.00, insert must be refused");

    assert!(
        err.to_string().contains("new row violates row-level security policy"),
        "expected policy violation, got: {err}"
    );

    let count: i64 = ledger2_rls::table.count().get_result(&mut conn).expect("count");
    assert_eq!(count, 0, "refused insert must store nothing");
}

/// A supplied `amount` that satisfies the policy must land unchanged.
#[test]
fn supplied_numeric_value_satisfies_check_policy() {
    let mut conn =
        apply("CREATE POLICY ledger2_ins ON ledger2 FOR INSERT WITH CHECK (amount < 5.00);");

    diesel::insert_into(ledger2::table)
        .values((ledger2::id.eq(3), ledger2::amount.eq(200i64), ledger2::owner.eq("alice")))
        .execute(&mut conn)
        .expect("supplied 2.00 < 5.00, insert must succeed");

    let stored: i64 = ledger2_rls::table
        .filter(ledger2_rls::id.eq(3))
        .select(ledger2_rls::amount)
        .first(&mut conn)
        .expect("row must exist");

    assert_eq!(stored, 200, "supplied value must be stored unchanged");
}
