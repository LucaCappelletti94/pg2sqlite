//! Execution contracts for caller-scoped exemption from generated write guards.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use diesel::{
    Connection as _, ExpressionMethods, QueryDsl, RunQueryDsl, SqliteConnection,
    connection::SimpleConnection,
};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

const WRITE_EXEMPTION_FUNCTION: &str = "write_is_exempt";

const POLICY_SCHEMA: &str = "
CREATE TABLE shared_items (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT ''
);
ALTER TABLE shared_items ENABLE ROW LEVEL SECURITY;
CREATE POLICY shared_items_select ON shared_items FOR SELECT USING (true);
CREATE POLICY shared_items_insert ON shared_items
    FOR INSERT WITH CHECK (owner = 'alice');
CREATE POLICY shared_items_update ON shared_items
    FOR UPDATE USING (owner = 'alice') WITH CHECK (owner = 'alice');
CREATE POLICY shared_items_delete ON shared_items
    FOR DELETE USING (owner = 'alice');
";

const READ_ONLY_SCHEMA: &str = "
CREATE ROLE app_user;
CREATE TABLE reference_items (
    id INTEGER PRIMARY KEY,
    body TEXT NOT NULL
);
GRANT SELECT ON reference_items TO app_user;
";

const READ_ONLY_RLS_SCHEMA: &str = "
CREATE ROLE app_user;
CREATE TABLE secure_reference_items (
    id INTEGER PRIMARY KEY,
    body TEXT NOT NULL
);
ALTER TABLE secure_reference_items ENABLE ROW LEVEL SECURITY;
CREATE POLICY secure_reference_items_select ON secure_reference_items
    FOR SELECT USING (true);
GRANT SELECT ON secure_reference_items TO app_user;
";

const TRIGGER_CHAIN_SCHEMA: &str = "
CREATE TABLE parent_items (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL
);
CREATE TABLE child_items (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    note TEXT NOT NULL
);
ALTER TABLE parent_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE child_items ENABLE ROW LEVEL SECURITY;
CREATE POLICY parent_items_select ON parent_items FOR SELECT USING (true);
CREATE POLICY parent_items_insert ON parent_items
    FOR INSERT WITH CHECK (owner = 'alice');
CREATE POLICY child_items_select ON child_items
    FOR SELECT USING (owner = 'alice');
CREATE POLICY child_items_insert ON child_items
    FOR INSERT WITH CHECK (owner = 'alice');
CREATE POLICY child_items_update ON child_items
    FOR UPDATE USING (owner = 'alice') WITH CHECK (owner = 'alice');
CREATE OR REPLACE FUNCTION touch_child() RETURNS TRIGGER AS $$
BEGIN
    UPDATE child_items SET note = 'touched' WHERE id = NEW.id;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER touch_child_after_parent
AFTER INSERT ON parent_items
FOR EACH ROW EXECUTE FUNCTION touch_child();
";

const FUNCTION_POLICY_SCHEMA: &str = "
CREATE TABLE function_items (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL
);
ALTER TABLE function_items ENABLE ROW LEVEL SECURITY;
CREATE POLICY function_items_select ON function_items FOR SELECT USING (true);
CREATE POLICY function_items_insert ON function_items
    FOR INSERT WITH CHECK (policy_allows(owner));
";

const RESTRICTIVE_SCHEMA: &str = "
CREATE TABLE restrictive_items (
    id INTEGER PRIMARY KEY,
    body TEXT NOT NULL
);
ALTER TABLE restrictive_items ENABLE ROW LEVEL SECURITY;
CREATE POLICY restrictive_insert ON restrictive_items AS RESTRICTIVE
    FOR INSERT WITH CHECK (true);
CREATE POLICY restrictive_update ON restrictive_items AS RESTRICTIVE
    FOR UPDATE USING (true) WITH CHECK (true);
CREATE POLICY restrictive_delete ON restrictive_items AS RESTRICTIVE
    FOR DELETE USING (true);
";

const ONE_SIDED_UPDATE_SCHEMA: &str = "
CREATE TABLE check_only_items (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    body TEXT NOT NULL
);
CREATE TABLE using_only_items (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    body TEXT NOT NULL
);
ALTER TABLE check_only_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE using_only_items ENABLE ROW LEVEL SECURITY;
CREATE POLICY check_only_select ON check_only_items FOR SELECT USING (true);
CREATE POLICY using_only_select ON using_only_items FOR SELECT USING (true);
CREATE POLICY check_only_insert ON check_only_items FOR INSERT WITH CHECK (true);
CREATE POLICY using_only_insert ON using_only_items FOR INSERT WITH CHECK (true);
CREATE POLICY check_only_update ON check_only_items FOR UPDATE
    USING (true) WITH CHECK (owner = 'alice');
CREATE POLICY using_only_update ON using_only_items FOR UPDATE
    USING (owner = 'alice') WITH CHECK (true);
";

mod schema {
    diesel::table! {
        shared_items (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        shared_items_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        reference_items (id) {
            id -> Integer,
            body -> Text,
        }
    }
    diesel::table! {
        secure_reference_items_rls (id) {
            id -> Integer,
            body -> Text,
        }
    }
    diesel::table! {
        parent_items_rls (id) {
            id -> Integer,
            owner -> Text,
        }
    }
    diesel::table! {
        child_items (id) {
            id -> Integer,
            owner -> Text,
            note -> Text,
        }
    }
    diesel::table! {
        child_items_rls (id) {
            id -> Integer,
            owner -> Text,
            note -> Text,
        }
    }
    diesel::table! {
        function_items_rls (id) {
            id -> Integer,
            owner -> Text,
        }
    }
    diesel::table! {
        restrictive_items_rls (id) {
            id -> Integer,
            body -> Text,
        }
    }
    diesel::table! {
        check_only_items_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        using_only_items_rls (id) {
            id -> Integer,
            owner -> Text,
            body -> Text,
        }
    }
    diesel::table! {
        rls_audit (id) {
            id -> Integer,
            table_name -> Text,
        }
    }
}

diesel::define_sql_function! {
    /// Reports whether generated write guards are exempt.
    fn write_is_exempt() -> diesel::sql_types::Bool;
}

diesel::define_sql_function! {
    /// Returns NULL to prove exemption fails closed.
    fn write_exemption_is_null() -> diesel::sql_types::Nullable<diesel::sql_types::Bool>;
}

diesel::define_sql_function! {
    /// Panics to prove exemption errors fail closed.
    fn write_exemption_panics() -> diesel::sql_types::Bool;
}

diesel::define_sql_function! {
    /// Reports whether a policy admits an owner.
    fn policy_allows(owner: diesel::sql_types::Text) -> diesel::sql_types::Bool;
}

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_strict_rls_write_deny()
        .with_write_exemption_function(WRITE_EXEMPTION_FUNCTION)
}

fn translated(pg: &str, options: &Pg2SqliteOptions) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse PostgreSQL")
        .translate_to_sql(options)
        .expect("translate SQLite")
}

fn apply(pg: &str, options: &Pg2SqliteOptions) -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    for statement in translated(pg, options) {
        connection.batch_execute(&statement).expect("apply translated statement");
    }
    connection
}

fn apply_with_exemption(
    pg: &str,
    options: &Pg2SqliteOptions,
) -> (SqliteConnection, Arc<AtomicBool>) {
    let mut connection = apply(pg, options);
    let exempt = Arc::new(AtomicBool::new(false));
    let function_state = Arc::clone(&exempt);
    write_is_exempt_utils::register_nondeterministic_impl(&mut connection, move || {
        function_state.load(Ordering::Relaxed)
    })
    .expect("register write exemption");
    (connection, exempt)
}

#[test]
fn null_and_function_error_fail_closed() {
    let null_options = options().with_write_exemption_function("write_exemption_is_null");
    let mut null_connection = apply(POLICY_SCHEMA, &null_options);
    write_exemption_is_null_utils::register_nondeterministic_impl(&mut null_connection, || {
        None::<bool>
    })
    .expect("register NULL exemption");
    assert!(
        diesel::insert_into(schema::shared_items_rls::table)
            .values((
                schema::shared_items_rls::id.eq(1),
                schema::shared_items_rls::owner.eq("bob"),
                schema::shared_items_rls::body.eq("blocked"),
            ))
            .execute(&mut null_connection)
            .is_err(),
        "NULL must enforce the write policy"
    );

    let panic_options = options().with_write_exemption_function("write_exemption_panics");
    let mut panic_connection = apply(POLICY_SCHEMA, &panic_options);
    write_exemption_panics_utils::register_nondeterministic_impl(
        &mut panic_connection,
        || -> bool { panic!("exemption failure") },
    )
    .expect("register failing exemption");
    assert!(
        diesel::insert_into(schema::shared_items_rls::table)
            .values((
                schema::shared_items_rls::id.eq(1),
                schema::shared_items_rls::owner.eq("bob"),
                schema::shared_items_rls::body.eq("blocked"),
            ))
            .execute(&mut panic_connection)
            .is_err(),
        "a function error must abort the write"
    );
}

#[test]
fn exemption_skips_registered_policy_evaluation_but_not_function_resolution() {
    let policy_options = options().with_user_defined_functions(["policy_allows"]);
    let (mut connection, exempt) = apply_with_exemption(FUNCTION_POLICY_SCHEMA, &policy_options);
    let calls = Arc::new(AtomicUsize::new(0));
    let function_calls = Arc::clone(&calls);
    policy_allows_utils::register_nondeterministic_impl(&mut connection, move |_owner: String| {
        function_calls.fetch_add(1, Ordering::Relaxed);
        false
    })
    .expect("register policy function");
    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::function_items_rls::table)
        .values((schema::function_items_rls::id.eq(1), schema::function_items_rls::owner.eq("bob")))
        .execute(&mut connection)
        .expect("exemption skips policy evaluation");
    assert_eq!(calls.load(Ordering::Relaxed), 0);

    let (mut missing_connection, missing_exempt) =
        apply_with_exemption(FUNCTION_POLICY_SCHEMA, &policy_options);
    missing_exempt.store(true, Ordering::Relaxed);
    let error = diesel::insert_into(schema::function_items_rls::table)
        .values((schema::function_items_rls::id.eq(2), schema::function_items_rls::owner.eq("bob")))
        .execute(&mut missing_connection)
        .expect_err("SQLite must resolve every policy function");
    assert!(error.to_string().contains("policy_allows"));
}

#[test]
fn exemption_covers_insert_update_and_delete_through_the_rls_view() {
    let (mut connection, exempt) = apply_with_exemption(POLICY_SCHEMA, &options());

    assert!(
        diesel::insert_into(schema::shared_items::table)
            .values((
                schema::shared_items::id.eq(1),
                schema::shared_items::owner.eq("bob"),
                schema::shared_items::body.eq("blocked"),
            ))
            .execute(&mut connection)
            .is_err()
    );

    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::shared_items::table)
        .values((
            schema::shared_items::id.eq(1),
            schema::shared_items::owner.eq("bob"),
            schema::shared_items::body.eq("inserted"),
        ))
        .execute(&mut connection)
        .expect("exempt view insert");

    exempt.store(false, Ordering::Relaxed);
    diesel::update(schema::shared_items::table.find(1))
        .set(schema::shared_items::body.eq("blocked"))
        .execute(&mut connection)
        .expect("policy-denied update is a no-op");
    assert_eq!(
        schema::shared_items_rls::table
            .find(1)
            .select(schema::shared_items_rls::body)
            .get_result::<String>(&mut connection)
            .expect("read blocked update result"),
        "inserted"
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::update(schema::shared_items::table.find(1))
        .set(schema::shared_items::body.eq("updated"))
        .execute(&mut connection)
        .expect("exempt view update");

    exempt.store(false, Ordering::Relaxed);
    diesel::delete(schema::shared_items::table.find(1))
        .execute(&mut connection)
        .expect("policy-denied delete is a no-op");
    assert_eq!(
        schema::shared_items_rls::table
            .count()
            .get_result::<i64>(&mut connection)
            .expect("count after blocked delete"),
        1
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::delete(schema::shared_items::table.find(1))
        .execute(&mut connection)
        .expect("exempt view delete");
    assert_eq!(
        schema::shared_items_rls::table
            .count()
            .get_result::<i64>(&mut connection)
            .expect("count backing rows"),
        0
    );
}

#[test]
fn backing_delete_enforces_policy_unless_exempt() {
    let (mut connection, exempt) = apply_with_exemption(POLICY_SCHEMA, &options());
    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::shared_items_rls::table)
        .values(&[
            (
                schema::shared_items_rls::id.eq(1),
                schema::shared_items_rls::owner.eq("bob"),
                schema::shared_items_rls::body.eq("first"),
            ),
            (
                schema::shared_items_rls::id.eq(2),
                schema::shared_items_rls::owner.eq("bob"),
                schema::shared_items_rls::body.eq("second"),
            ),
        ])
        .execute(&mut connection)
        .expect("seed backing rows");

    exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::delete(schema::shared_items_rls::table.find(1)).execute(&mut connection).is_err(),
        "normal backing delete must enforce policy"
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::delete(schema::shared_items_rls::table.find(2))
        .execute(&mut connection)
        .expect("exempt backing delete");
}

#[test]
fn read_only_non_rls_guards_honour_exemption() {
    let role_options = Pg2SqliteOptions::default()
        .with_session_user_role("app_user")
        .with_write_exemption_function(WRITE_EXEMPTION_FUNCTION);
    let (mut connection, exempt) = apply_with_exemption(READ_ONLY_SCHEMA, &role_options);

    assert!(
        diesel::insert_into(schema::reference_items::table)
            .values((
                schema::reference_items::id.eq(1),
                schema::reference_items::body.eq("blocked")
            ))
            .execute(&mut connection)
            .is_err()
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::reference_items::table)
        .values((schema::reference_items::id.eq(1), schema::reference_items::body.eq("inserted")))
        .execute(&mut connection)
        .expect("exempt read-only insert");
    exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::update(schema::reference_items::table.find(1))
            .set(schema::reference_items::body.eq("blocked"))
            .execute(&mut connection)
            .is_err()
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::update(schema::reference_items::table.find(1))
        .set(schema::reference_items::body.eq("updated"))
        .execute(&mut connection)
        .expect("exempt read-only update");
    exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::delete(schema::reference_items::table.find(1)).execute(&mut connection).is_err()
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::delete(schema::reference_items::table.find(1))
        .execute(&mut connection)
        .expect("exempt read-only delete");
}

#[test]
fn read_only_rls_backing_guards_honour_exemption() {
    let role_options = Pg2SqliteOptions::default()
        .with_session_user_role("app_user")
        .with_rls_audit_table_name("rls_audit")
        .with_write_exemption_function(WRITE_EXEMPTION_FUNCTION);
    let (mut connection, exempt) = apply_with_exemption(READ_ONLY_RLS_SCHEMA, &role_options);

    assert!(
        diesel::insert_into(schema::secure_reference_items_rls::table)
            .values((
                schema::secure_reference_items_rls::id.eq(1),
                schema::secure_reference_items_rls::body.eq("blocked"),
            ))
            .execute(&mut connection)
            .is_err()
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::secure_reference_items_rls::table)
        .values((
            schema::secure_reference_items_rls::id.eq(1),
            schema::secure_reference_items_rls::body.eq("inserted"),
        ))
        .execute(&mut connection)
        .expect("exempt read-only RLS insert");
    exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::update(schema::secure_reference_items_rls::table.find(1))
            .set(schema::secure_reference_items_rls::body.eq("blocked"))
            .execute(&mut connection)
            .is_err()
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::update(schema::secure_reference_items_rls::table.find(1))
        .set(schema::secure_reference_items_rls::body.eq("updated"))
        .execute(&mut connection)
        .expect("exempt read-only RLS update");
    exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::delete(schema::secure_reference_items_rls::table.find(1))
            .execute(&mut connection)
            .is_err()
    );
    exempt.store(true, Ordering::Relaxed);
    diesel::delete(schema::secure_reference_items_rls::table.find(1))
        .execute(&mut connection)
        .expect("exempt read-only RLS delete");
}

#[test]
fn translated_trigger_writes_backing_storage_while_reads_stay_filtered() {
    let (mut connection, exempt) = apply_with_exemption(TRIGGER_CHAIN_SCHEMA, &options());
    exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::child_items_rls::table)
        .values((
            schema::child_items_rls::id.eq(1),
            schema::child_items_rls::owner.eq("bob"),
            schema::child_items_rls::note.eq("original"),
        ))
        .execute(&mut connection)
        .expect("seed hidden child");
    diesel::insert_into(schema::parent_items_rls::table)
        .values((schema::parent_items_rls::id.eq(1), schema::parent_items_rls::owner.eq("bob")))
        .execute(&mut connection)
        .expect("insert parent and run translated trigger");

    assert_eq!(
        schema::child_items_rls::table
            .find(1)
            .select(schema::child_items_rls::note)
            .get_result::<String>(&mut connection)
            .expect("read child backing row"),
        "touched"
    );
    assert_eq!(
        schema::child_items::table
            .count()
            .get_result::<i64>(&mut connection)
            .expect("count visible children"),
        0,
        "ordinary reads must remain filtered"
    );
    assert!(
        schema::rls_audit::table
            .count()
            .get_result::<i64>(&mut connection)
            .expect("count monitoring rows")
            >= 1,
        "monitoring must stay active"
    );
}

#[test]
fn restrictive_and_one_sided_update_guards_honour_exemption() {
    let (mut restrictive_connection, restrictive_exempt) =
        apply_with_exemption(RESTRICTIVE_SCHEMA, &options());
    assert!(
        diesel::insert_into(schema::restrictive_items_rls::table)
            .values((
                schema::restrictive_items_rls::id.eq(1),
                schema::restrictive_items_rls::body.eq("blocked"),
            ))
            .execute(&mut restrictive_connection)
            .is_err()
    );
    restrictive_exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::restrictive_items_rls::table)
        .values((
            schema::restrictive_items_rls::id.eq(1),
            schema::restrictive_items_rls::body.eq("inserted"),
        ))
        .execute(&mut restrictive_connection)
        .expect("exempt restrictive-only insert");
    restrictive_exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::update(schema::restrictive_items_rls::table.find(1))
            .set(schema::restrictive_items_rls::body.eq("blocked"))
            .execute(&mut restrictive_connection)
            .is_err()
    );
    restrictive_exempt.store(true, Ordering::Relaxed);
    diesel::update(schema::restrictive_items_rls::table.find(1))
        .set(schema::restrictive_items_rls::body.eq("updated"))
        .execute(&mut restrictive_connection)
        .expect("exempt restrictive-only update");
    restrictive_exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::delete(schema::restrictive_items_rls::table.find(1))
            .execute(&mut restrictive_connection)
            .is_err()
    );
    restrictive_exempt.store(true, Ordering::Relaxed);
    diesel::delete(schema::restrictive_items_rls::table.find(1))
        .execute(&mut restrictive_connection)
        .expect("exempt restrictive-only delete");

    let (mut one_sided_connection, one_sided_exempt) =
        apply_with_exemption(ONE_SIDED_UPDATE_SCHEMA, &options());
    one_sided_exempt.store(true, Ordering::Relaxed);
    diesel::insert_into(schema::check_only_items_rls::table)
        .values((
            schema::check_only_items_rls::id.eq(1),
            schema::check_only_items_rls::owner.eq("bob"),
            schema::check_only_items_rls::body.eq("old"),
        ))
        .execute(&mut one_sided_connection)
        .expect("seed check-only row");
    diesel::insert_into(schema::using_only_items_rls::table)
        .values((
            schema::using_only_items_rls::id.eq(1),
            schema::using_only_items_rls::owner.eq("bob"),
            schema::using_only_items_rls::body.eq("old"),
        ))
        .execute(&mut one_sided_connection)
        .expect("seed using-only row");
    one_sided_exempt.store(false, Ordering::Relaxed);
    assert!(
        diesel::update(schema::check_only_items_rls::table.find(1))
            .set(schema::check_only_items_rls::body.eq("blocked"))
            .execute(&mut one_sided_connection)
            .is_err()
    );
    assert!(
        diesel::update(schema::using_only_items_rls::table.find(1))
            .set(schema::using_only_items_rls::body.eq("blocked"))
            .execute(&mut one_sided_connection)
            .is_err()
    );
    one_sided_exempt.store(true, Ordering::Relaxed);
    diesel::update(schema::check_only_items_rls::table.find(1))
        .set(schema::check_only_items_rls::body.eq("updated"))
        .execute(&mut one_sided_connection)
        .expect("exempt check-only update");
    diesel::update(schema::using_only_items_rls::table.find(1))
        .set(schema::using_only_items_rls::body.eq("updated"))
        .execute(&mut one_sided_connection)
        .expect("exempt using-only update");
}

#[test]
fn hardened_connection_accepts_only_innocuous_exemption_function() {
    use rusqlite::functions::FunctionFlags;

    let translated = translated(
        POLICY_SCHEMA,
        &Pg2SqliteOptions::default()
            .with_rls_audit_table_name("rls_audit")
            .with_write_exemption_function(WRITE_EXEMPTION_FUNCTION),
    );
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS;
    let connection = rusqlite::Connection::open_in_memory().expect("open hardened SQLite");
    connection
        .create_scalar_function(WRITE_EXEMPTION_FUNCTION, 0, flags, |_| Ok(true))
        .expect("register innocuous exemption");
    connection.execute_batch("PRAGMA trusted_schema = OFF").expect("disable trusted schema");
    for statement in &translated {
        connection.execute_batch(statement).expect("apply hardened DDL");
    }
    // rusqlite is required here because the project's Diesel version cannot set
    // SQLITE_INNOCUOUS.
    connection
        .execute("INSERT INTO shared_items_rls (id, owner, body) VALUES (1, 'bob', 'server')", [])
        .expect("innocuous exemption runs from the trigger");

    let unsafe_connection = rusqlite::Connection::open_in_memory().expect("open unsafe SQLite");
    unsafe_connection
        .create_scalar_function(WRITE_EXEMPTION_FUNCTION, 0, FunctionFlags::SQLITE_UTF8, |_| {
            Ok(true)
        })
        .expect("register non-innocuous exemption");
    unsafe_connection.execute_batch("PRAGMA trusted_schema = OFF").expect("disable trusted schema");
    for statement in &translated {
        unsafe_connection.execute_batch(statement).expect("apply unsafe DDL");
    }
    let error = unsafe_connection
        .execute("INSERT INTO shared_items_rls (id, owner, body) VALUES (1, 'bob', 'server')", [])
        .expect_err("non-innocuous schema function must fail");
    assert!(error.to_string().contains("unsafe use"));
}

#[test]
fn quoted_exemption_function_name_is_safe() {
    const QUOTED_NAME: &str = "select";
    let quoted_options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("rls_audit")
        .with_write_exemption_function(QUOTED_NAME);
    let mut connection = apply(POLICY_SCHEMA, &quoted_options);
    connection
        .register_noarg_sql_function::<diesel::sql_types::Bool, bool, _>(
            QUOTED_NAME,
            diesel::sqlite::SqliteFunctionBehavior::empty(),
            || true,
        )
        .expect("register quoted function name");
    diesel::insert_into(schema::shared_items_rls::table)
        .values((
            schema::shared_items_rls::id.eq(1),
            schema::shared_items_rls::owner.eq("bob"),
            schema::shared_items_rls::body.eq("server"),
        ))
        .execute(&mut connection)
        .expect("quoted exemption function runs");
}
