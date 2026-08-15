//! Writing through the view a policy-bearing table becomes must land the same
//! row writing the unsplit table would.
//!
//! A SQLite view carries no column defaults, so a column the caller omits
//! reaches the `INSTEAD OF INSERT` trigger as NULL. Forwarding that NULL
//! overrides the backing table's own `DEFAULT`, which is why a client-minted
//! primary key used to fail with a not-null violation. The trigger therefore
//! reproduces each column's emitted default itself, and its `WITH CHECK` guard
//! reads the same defaulted value the row will carry, or a default the policy
//! forbids would slip past the guard.
//!
//! SQLite cannot tell an omitted column from an explicitly written NULL, so the
//! default wins for both. `an_explicit_null_takes_the_default` pins that
//! accepted divergence.
//!
//! Stored generated columns are the mirror case: SQLite refuses to be told
//! their value, so the trigger neither forwards nor assigns them, and a caller
//! who supplies one is refused the way PostgreSQL refuses it.

mod helpers;

use diesel::prelude::*;
use helpers::{establish_connection, set_session_user_id};
use pg2sqlite::prelude::{
    Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping, TranslationOptions, UuidRepresentation,
};
use rosetta_uuid::Uuid;

mod schema {
    diesel::table! {
        /// The view callers write through.
        docs (id) {
            id -> Text,
            owner -> Text,
            note -> Nullable<Text>,
            price -> Nullable<BigInt>,
            qty -> Integer,
        }
    }

    diesel::table! {
        /// The backing table, read directly so assertions see what was stored.
        docs_rls (id) {
            id -> Text,
            owner -> Text,
            note -> Nullable<Text>,
            price -> Nullable<BigInt>,
            qty -> Integer,
        }
    }

    diesel::table! {
        /// A view over a table carrying a computed column.
        items (id) {
            id -> Integer,
            qty -> Integer,
            owner -> Text,
            doubled -> Integer,
        }
    }

    diesel::table! {
        /// Backing table for `items`.
        items_rls (id) {
            id -> Integer,
            qty -> Integer,
            owner -> Text,
            doubled -> Integer,
        }
    }

    diesel::table! {
        /// The shape a synced client writes: the key is minted locally from the
        /// schema's own default.
        orders (id) {
            id -> Binary,
            owner_id -> Binary,
            quantity -> BigInt,
        }
    }

    diesel::table! {
        /// Backing table for `orders`.
        orders_rls (id) {
            id -> Binary,
            owner_id -> Binary,
            quantity -> BigInt,
        }
    }
}

use schema::{docs, docs_rls, items, items_rls, orders, orders_rls};

/// Every column but `qty` declares a default, and the policy reads a defaulted
/// column.
const DOCS: &str = "
    CREATE TABLE docs (
        id TEXT PRIMARY KEY DEFAULT 'minted' NOT NULL,
        owner TEXT NOT NULL DEFAULT 'alice',
        note TEXT DEFAULT 'unset',
        price NUMERIC(10,2) DEFAULT 1.5,
        qty INTEGER NOT NULL
    );
    ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
    CREATE POLICY docs_p ON docs USING (owner = 'alice');
";

/// Same table, but the policy forbids what the `owner` default supplies.
const DOCS_HOSTILE_DEFAULT: &str = "
    CREATE TABLE docs (
        id TEXT PRIMARY KEY DEFAULT 'minted' NOT NULL,
        owner TEXT NOT NULL DEFAULT 'alice',
        note TEXT DEFAULT 'unset',
        price NUMERIC(10,2) DEFAULT 1.5,
        qty INTEGER NOT NULL
    );
    ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
    CREATE POLICY docs_p ON docs USING (owner = 'bob');
";

const ITEMS: &str = "
    CREATE TABLE items (
        id INTEGER PRIMARY KEY,
        qty INTEGER NOT NULL,
        owner TEXT NOT NULL,
        doubled INTEGER GENERATED ALWAYS AS (qty * 2) STORED
    );
    ALTER TABLE items ENABLE ROW LEVEL SECURITY;
    CREATE POLICY items_p ON items USING (owner = 'alice');
";

/// The policy reads the computed column, and the column the computation reads
/// declares a default, so the guard has to compute from the defaulted value.
const GUARDED_BY_COMPUTED: &str = "
    CREATE TABLE items (
        id INTEGER PRIMARY KEY,
        qty INTEGER NOT NULL DEFAULT 4,
        owner TEXT NOT NULL,
        doubled INTEGER GENERATED ALWAYS AS (qty * 2) STORED
    );
    ALTER TABLE items ENABLE ROW LEVEL SECURITY;
    CREATE POLICY items_p ON items USING (doubled < 100);
";

const ORDERS: &str = "
    CREATE TABLE orders (
        id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
        owner_id UUID NOT NULL,
        quantity BIGINT NOT NULL
    );
    ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
    CREATE POLICY orders_p ON orders USING (owner_id = current_setting('app.user_id'));
";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_uuid_representation(UuidRepresentation::Blob)
        .with_uuid_function_name("uuidv7".to_owned())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
}

/// Applies the translated DDL. The emitted SQL is the artifact under test, so
/// it runs as generated text; every other statement uses the typed DSL.
fn apply(pg: &str) -> SqliteConnection {
    let translated =
        Pg2Sqlite::default().sql(pg).expect("parse").translate(&options()).expect("translate");

    let mut conn = establish_connection();
    for statement in &translated {
        diesel::sql_query(statement.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|error| panic!("emitted DDL failed: {error}\n{statement}"));
    }
    conn
}

/// The one case the bug report opened with: a caller naming neither the key nor
/// any other defaulted column lands a row carrying every default.
#[test]
fn an_omitted_column_takes_its_default() {
    let mut conn = apply(DOCS);

    diesel::insert_into(docs::table)
        .values((docs::owner.eq("alice"), docs::qty.eq(7)))
        .execute(&mut conn)
        .expect("an insert omitting defaulted columns must succeed");

    let stored: (String, String, Option<String>, Option<i64>, i32) = docs_rls::table
        .select((docs_rls::id, docs_rls::owner, docs_rls::note, docs_rls::price, docs_rls::qty))
        .first(&mut conn)
        .expect("the row must exist in the backing table");

    assert_eq!(
        stored,
        ("minted".to_owned(), "alice".to_owned(), Some("unset".to_owned()), Some(150), 7),
        "each omitted column must carry the default the backing table declares, and a NUMERIC \
         default must arrive in the minor units the column stores"
    );
}

/// The fallback must not shadow a value the caller did supply.
#[test]
fn a_supplied_value_beats_the_default() {
    let mut conn = apply(DOCS);

    diesel::insert_into(docs::table)
        .values((
            docs::id.eq("given"),
            docs::owner.eq("alice"),
            docs::note.eq("written"),
            docs::price.eq(999),
            docs::qty.eq(1),
        ))
        .execute(&mut conn)
        .expect("insert");

    let stored: (String, Option<String>, Option<i64>) = docs_rls::table
        .select((docs_rls::id, docs_rls::note, docs_rls::price))
        .first(&mut conn)
        .expect("row");

    assert_eq!(stored, ("given".to_owned(), Some("written".to_owned()), Some(999)));
}

/// Characterization test for the accepted divergence: a view gives the trigger
/// no way to tell an omitted column from one explicitly set to NULL, so the
/// default wins for both. PostgreSQL, which applies the default only to the
/// omitted column, would store NULL here.
#[test]
fn an_explicit_null_takes_the_default() {
    let mut conn = apply(DOCS);

    diesel::insert_into(docs::table)
        .values((
            docs::id.eq("explicit"),
            docs::owner.eq("alice"),
            docs::note.eq(None::<String>),
            docs::qty.eq(1),
        ))
        .execute(&mut conn)
        .expect("insert");

    let note: Option<String> =
        docs_rls::table.select(docs_rls::note).first(&mut conn).expect("row");

    assert_eq!(
        note,
        Some("unset".to_owned()),
        "an explicit NULL is indistinguishable from an omitted column, so it takes the default"
    );
}

/// The guard has to judge the row as it will be stored. A defaulted column the
/// policy admits must not be read as NULL, which satisfies no predicate.
#[test]
fn a_default_the_policy_admits_lands_and_is_visible() {
    let mut conn = apply(DOCS);

    diesel::insert_into(docs::table)
        .values((docs::id.eq("visible"), docs::qty.eq(3)))
        .execute(&mut conn)
        .expect("the defaulted owner satisfies the policy, so the insert must succeed");

    let seen: Vec<String> =
        docs::table.select(docs::owner).load(&mut conn).expect("read through the view");

    assert_eq!(seen, vec!["alice".to_owned()], "the row must be visible through the view");
}

/// The other half of the same rule: a default the policy forbids must be
/// refused as a policy violation, not slip through because the guard saw NULL.
#[test]
fn a_default_the_policy_forbids_is_refused() {
    let mut conn = apply(DOCS_HOSTILE_DEFAULT);

    let refused = diesel::insert_into(docs::table)
        .values((docs::id.eq("hostile"), docs::qty.eq(3)))
        .execute(&mut conn);

    let error = refused.expect_err("the defaulted owner violates the policy").to_string();
    assert!(
        error.contains("new row violates row-level security policy"),
        "the guard must judge the defaulted value, got: {error}"
    );

    let count: i64 = docs_rls::table.count().get_result(&mut conn).expect("count");
    assert_eq!(count, 0, "the refused insert must store nothing");
}

/// An UPDATE names the row that already exists, and SQLite fills `NEW` from it,
/// so no column is ever missing and defaults must stay out of that path.
#[test]
fn an_update_does_not_apply_defaults() {
    let mut conn = apply(DOCS);

    diesel::insert_into(docs::table)
        .values((
            docs::id.eq("row"),
            docs::owner.eq("alice"),
            docs::note.eq("kept"),
            docs::qty.eq(1),
        ))
        .execute(&mut conn)
        .expect("insert");

    diesel::update(docs::table.filter(docs::id.eq("row")))
        .set(docs::note.eq(None::<String>))
        .execute(&mut conn)
        .expect("update");

    let note: Option<String> =
        docs_rls::table.select(docs_rls::note).first(&mut conn).expect("row");

    assert_eq!(note, None, "an update clearing a column must store NULL, not the default");
}

/// A computed column made the whole table unwritable through its view: the
/// trigger named it, and SQLite refuses to be told a generated column's value.
#[test]
fn a_computed_column_does_not_block_a_write() {
    let mut conn = apply(ITEMS);

    diesel::insert_into(items::table)
        .values((items::id.eq(1), items::qty.eq(4), items::owner.eq("alice")))
        .execute(&mut conn)
        .expect("an insert into a table with a computed column must succeed");

    let stored: (i32, i32) = items_rls::table
        .select((items_rls::qty, items_rls::doubled))
        .first(&mut conn)
        .expect("row");

    assert_eq!(stored, (4, 8), "SQLite must compute the generated column itself");
}

/// PostgreSQL refuses a non-default value for a generated column, so the view
/// must refuse it too rather than dropping it in silence.
#[test]
fn writing_a_computed_column_through_the_view_is_refused() {
    let mut conn = apply(ITEMS);

    let refused = diesel::insert_into(items::table)
        .values((
            items::id.eq(1),
            items::qty.eq(4),
            items::owner.eq("alice"),
            items::doubled.eq(99),
        ))
        .execute(&mut conn);

    let error = refused.expect_err("a value for a computed column must be refused").to_string();
    assert!(
        error.contains(r#"cannot write to generated column "doubled""#),
        "the trigger's own guard must be what refuses it, got: {error}"
    );
}

/// The UPDATE trigger assigns every column, so it hit the same wall.
#[test]
fn an_update_recomputes_a_computed_column() {
    let mut conn = apply(ITEMS);

    diesel::insert_into(items::table)
        .values((items::id.eq(1), items::qty.eq(4), items::owner.eq("alice")))
        .execute(&mut conn)
        .expect("insert");

    diesel::update(items::table.filter(items::id.eq(1)))
        .set(items::qty.eq(10))
        .execute(&mut conn)
        .expect("an update of a table with a computed column must succeed");

    let stored: (i32, i32) = items_rls::table
        .select((items_rls::qty, items_rls::doubled))
        .first(&mut conn)
        .expect("row");

    assert_eq!(stored, (10, 20), "the generated column must follow the column it derives from");
}

/// Same refusal on the update path.
#[test]
fn setting_a_computed_column_through_the_view_is_refused() {
    let mut conn = apply(ITEMS);

    diesel::insert_into(items::table)
        .values((items::id.eq(1), items::qty.eq(4), items::owner.eq("alice")))
        .execute(&mut conn)
        .expect("insert");

    let refused = diesel::update(items::table.filter(items::id.eq(1)))
        .set(items::doubled.eq(99))
        .execute(&mut conn);

    let error = refused.expect_err("assigning a computed column must be refused").to_string();
    assert!(
        error.contains(r#"cannot write to generated column "doubled""#),
        "the trigger's own guard must be what refuses it, got: {error}"
    );
}

/// The reported shape end to end: a client mints its own key from the schema's
/// own default, with the policy resolved through a session function.
#[test]
fn a_client_minted_key_lands_through_the_view() {
    let user = Uuid::utc_v7();
    set_session_user_id(&user);
    let user_bytes = <[u8; 16]>::from(user).to_vec();
    let mut conn = apply(ORDERS);

    diesel::insert_into(orders::table)
        .values((orders::owner_id.eq(user_bytes.clone()), orders::quantity.eq(7_i64)))
        .execute(&mut conn)
        .expect("an insert leaving the key to the schema's default must succeed");

    let stored: (Vec<u8>, Vec<u8>, i64) = orders_rls::table
        .select((orders_rls::id, orders_rls::owner_id, orders_rls::quantity))
        .first(&mut conn)
        .expect("row");

    assert_eq!(stored.0.len(), 16, "the default must mint a UUID, got {:?}", stored.0);
    assert_eq!((stored.1, stored.2), (user_bytes, 7));

    let visible: i64 = orders::table.count().get_result(&mut conn).expect("count");
    assert_eq!(visible, 1, "the minted row must be visible through the view");
}

/// A policy reading a computed column has to judge the value SQLite will
/// compute. The view cannot supply it, so the guard computes it from the
/// columns the row carries, defaults included.
#[test]
fn a_policy_over_a_computed_column_refuses_a_row_it_forbids() {
    let mut conn = apply(GUARDED_BY_COMPUTED);

    let refused = diesel::insert_into(items::table)
        .values((items::id.eq(1), items::qty.eq(200), items::owner.eq("alice")))
        .execute(&mut conn);

    let error = refused.expect_err("doubled would be 400, which the policy forbids").to_string();
    assert!(
        error.contains("new row violates row-level security policy"),
        "the guard must compute the generated value rather than read NULL, got: {error}"
    );

    let count: i64 = items_rls::table.count().get_result(&mut conn).expect("count");
    assert_eq!(count, 0, "the refused insert must store nothing");
}

/// The same policy admits a row whose computed value satisfies it, including
/// when the column the computation reads came from its own default.
#[test]
fn a_policy_over_a_computed_column_admits_a_row_it_allows() {
    let mut conn = apply(GUARDED_BY_COMPUTED);

    diesel::insert_into(items::table)
        .values((items::id.eq(1), items::owner.eq("alice")))
        .execute(&mut conn)
        .expect("qty defaults to 4, so doubled is 8 and the policy admits it");

    let stored: (i32, i32) = items_rls::table
        .select((items_rls::qty, items_rls::doubled))
        .first(&mut conn)
        .expect("row");
    assert_eq!(stored, (4, 8));

    let visible: i64 = items::table.count().get_result(&mut conn).expect("count");
    assert_eq!(visible, 1, "the row must be visible through the view");
}

/// An update that pushes the computed value out of the policy is a violation,
/// not a row that silently disappears from the view.
#[test]
fn an_update_pushing_a_computed_value_out_of_the_policy_is_refused() {
    let mut conn = apply(GUARDED_BY_COMPUTED);

    diesel::insert_into(items::table)
        .values((items::id.eq(1), items::qty.eq(5), items::owner.eq("alice")))
        .execute(&mut conn)
        .expect("insert");

    let refused = diesel::update(items::table.filter(items::id.eq(1)))
        .set(items::qty.eq(300))
        .execute(&mut conn);

    let error = refused.expect_err("doubled would become 600").to_string();
    assert!(
        error.contains("new row violates row-level security policy"),
        "the guard must recompute the generated value, got: {error}"
    );

    let stored: i32 = items_rls::table.select(items_rls::qty).first(&mut conn).expect("row");
    assert_eq!(stored, 5, "the refused update must leave the row alone");
}

/// An update that keeps the computed value inside the policy still succeeds.
#[test]
fn an_update_keeping_a_computed_value_inside_the_policy_succeeds() {
    let mut conn = apply(GUARDED_BY_COMPUTED);

    diesel::insert_into(items::table)
        .values((items::id.eq(1), items::qty.eq(5), items::owner.eq("alice")))
        .execute(&mut conn)
        .expect("insert");

    diesel::update(items::table.filter(items::id.eq(1)))
        .set(items::qty.eq(6))
        .execute(&mut conn)
        .expect("doubled becomes 12, still inside the policy");

    let stored: (i32, i32) = items_rls::table
        .select((items_rls::qty, items_rls::doubled))
        .first(&mut conn)
        .expect("row");
    assert_eq!(stored, (6, 12));
}
