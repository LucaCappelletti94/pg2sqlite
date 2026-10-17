//! SQL-function calls preserve parameter bindings and row-security predicates.

use diesel::{prelude::*, sqlite::SqliteConnection};
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};

mod schema {
    diesel::table! {
        docs (id) {
            id -> Integer,
        }
    }
    diesel::table! {
        orders (id) {
            id -> Text,
            owner_id -> Text,
            quantity -> Nullable<Integer>,
        }
    }
    diesel::table! {
        orders_rls (id) {
            id -> Text,
            owner_id -> Text,
            quantity -> Nullable<Integer>,
        }
    }
    diesel::table! {
        shares (owner_id, grantee) {
            owner_id -> Text,
            grantee -> Text,
        }
    }
    diesel::table! {
        shares_rls (owner_id, grantee) {
            owner_id -> Text,
            grantee -> Text,
        }
    }
}

diesel::define_sql_function! {
    /// Returns the replica caller.
    fn app_user() -> diesel::sql_types::Text;
}
diesel::define_sql_function! {
    /// Identifies trusted replica loading.
    fn replica_write() -> diesel::sql_types::Bool;
}

const SHARE_SCHEMA: &str = "
CREATE TABLE orders (id TEXT PRIMARY KEY, owner_id TEXT NOT NULL, quantity INT);
ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
CREATE TABLE shares (owner_id TEXT NOT NULL, grantee TEXT NOT NULL,
    PRIMARY KEY (owner_id, grantee));
ALTER TABLE shares ENABLE ROW LEVEL SECURITY;
CREATE FUNCTION shared_with_user(o TEXT) RETURNS BOOLEAN LANGUAGE sql SECURITY DEFINER
SET search_path TO public, pg_catalog, pg_temp AS
'SELECT EXISTS (SELECT 1 FROM shares s WHERE s.owner_id = o
    AND s.grantee = current_setting(''app.user_id'', true))';
CREATE POLICY shares_owner ON shares FOR ALL
    USING (owner_id = current_setting('app.user_id', true))
    WITH CHECK (owner_id = current_setting('app.user_id', true));
CREATE POLICY shares_grantee ON shares FOR SELECT
    USING (grantee = current_setting('app.user_id', true));
CREATE POLICY orders_owner ON orders FOR ALL
    USING (owner_id = current_setting('app.user_id', true))
    WITH CHECK (owner_id = current_setting('app.user_id', true));
CREATE POLICY orders_user_share ON orders FOR SELECT USING (shared_with_user(owner_id));
";

fn share_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_rls_audit_table_name("audit")
        .with_session_variable(SessionVariableMapping::current_setting("app.user_id", "app_user"))
        .with_user_defined_functions(["app_user"])
        .with_write_exemption_function("replica_write")
}

fn share_connection(sql: &str, caller: &'static str) -> SqliteConnection {
    let report = Pg2Sqlite::default()
        .sql(sql)
        .expect("parse share policy")
        .translate_with_report(&share_options())
        .expect("inline share helper");
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    app_user_utils::register_impl(&mut connection, move || caller.to_owned())
        .expect("register caller");
    replica_write_utils::register_impl(&mut connection, || true).expect("enable replica loading");
    for statement in report.statements {
        // Generated views and triggers require the emitted DDL.
        diesel::sql_query(statement.to_string()).execute(&mut connection).expect("apply schema");
    }
    use schema::{orders_rls, shares_rls};
    diesel::insert_into(orders_rls::table)
        .values([
            (orders_rls::id.eq("alice-order"), orders_rls::owner_id.eq("alice")),
            (orders_rls::id.eq("bob-order"), orders_rls::owner_id.eq("bob")),
            (orders_rls::id.eq("eve-order"), orders_rls::owner_id.eq("eve")),
        ])
        .execute(&mut connection)
        .expect("load replica orders");
    diesel::insert_into(shares_rls::table)
        .values((shares_rls::owner_id.eq("bob"), shares_rls::grantee.eq("alice")))
        .execute(&mut connection)
        .expect("load replica shares");
    replica_write_utils::register_impl(&mut connection, || false).expect("enforce write policies");
    connection
}

fn order_ids(connection: &mut SqliteConnection) -> Vec<String> {
    use schema::orders;
    orders::table.select(orders::id).order(orders::id).load(connection).expect("read orders")
}

#[test]
fn policy_helper_grants_only_owned_or_shared_orders() {
    let mut alice = share_connection(SHARE_SCHEMA, "alice");
    assert_eq!(order_ids(&mut alice), ["alice-order", "bob-order"]);
    let mut bob = share_connection(SHARE_SCHEMA, "bob");
    assert_eq!(order_ids(&mut bob), ["bob-order"]);
    let mut eve = share_connection(SHARE_SCHEMA, "eve");
    assert_eq!(order_ids(&mut eve), ["eve-order"]);
}

#[test]
fn definer_reads_delivered_shares_hidden_by_the_callers_policy() {
    let sql = SHARE_SCHEMA.replace(
        "CREATE POLICY shares_grantee ON shares FOR SELECT\n    USING (grantee = current_setting('app.user_id', true));",
        "",
    );
    let mut alice = share_connection(&sql, "alice");
    assert_eq!(schema::shares::table.count().get_result::<i64>(&mut alice).unwrap(), 0);
    assert_eq!(order_ids(&mut alice), ["alice-order", "bob-order"]);
    let sql = sql.replace("SECURITY DEFINER", "SECURITY INVOKER");
    let mut invoker = share_connection(&sql, "alice");
    assert_eq!(order_ids(&mut invoker), ["alice-order"]);
}

#[derive(QueryableByName)]
struct IntegerResult {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
    result: Option<i64>,
}

fn integer_result(sql: &str) -> Option<i64> {
    integer_result_with_options(sql, &Pg2SqliteOptions::default())
}

fn integer_result_with_options(sql: &str, options: &Pg2SqliteOptions) -> Option<i64> {
    let statements = Pg2Sqlite::default()
        .sql(sql)
        .expect("parse expression")
        .translate(options)
        .expect("inline expression");
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    app_user_utils::register_impl(&mut connection, || "bob".to_owned()).unwrap();
    let mut result = None;
    for statement in statements {
        if matches!(statement, sqlparser::ast::Statement::Query(_)) {
            // The translated expression is the runtime input under test.
            result = diesel::sql_query(statement.to_string())
                .get_result::<IntegerResult>(&mut connection)
                .expect("execute translated expression")
                .result;
        } else {
            // The translator emits DDL and connection pragmas.
            diesel::sql_query(statement.to_string()).execute(&mut connection).unwrap();
        }
    }
    result
}

#[test]
fn arguments_and_returned_expressions_keep_arithmetic_precedence() {
    assert_eq!(
        integer_result(
            "CREATE FUNCTION times_four(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x * 4';
             SELECT times_four(2 + 3) * 2 AS result;"
        ),
        Some(40)
    );
    assert_eq!(
        integer_result(
            "CREATE FUNCTION plus_one(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x + 1';
             SELECT plus_one(3) * 2 AS result;"
        ),
        Some(8)
    );
}

#[test]
fn positional_parameters_and_nested_helpers_expand() {
    assert_eq!(
        integer_result(
            "CREATE FUNCTION plus_one(INT) RETURNS INT LANGUAGE sql AS 'SELECT $1 + 1';
             CREATE FUNCTION twice(x INT) RETURNS INT LANGUAGE sql AS 'SELECT plus_one(x) * 2';
             SELECT twice(3) AS result;"
        ),
        Some(8)
    );
    assert_eq!(
        integer_result(
            "CREATE FUNCTION answer() RETURNS INT LANGUAGE sql AS $$SELECT 42$$;
             SELECT answer() AS result;"
        ),
        Some(42)
    );
}

#[test]
fn function_identity_preserves_schema_and_quoting() {
    assert_eq!(
        integer_result(
            r#"CREATE SCHEMA app;
               CREATE FUNCTION app."Step"(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x + 1';
               SELECT app."Step"(4) AS result;"#
        ),
        Some(5)
    );
    let error = Pg2Sqlite::default()
        .sql(
            r#"CREATE FUNCTION "Step"(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x + 1';
               SELECT step(4);"#,
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
}

#[test]
fn body_columns_shadow_named_parameters_but_not_positional_parameters() {
    assert_eq!(
        integer_result(
            "CREATE TABLE marks (x INTEGER PRIMARY KEY);
             INSERT INTO marks VALUES (9);
             CREATE FUNCTION matches_mark(x INT) RETURNS BOOLEAN LANGUAGE sql
                 AS 'SELECT EXISTS (SELECT 1 FROM marks WHERE x = 9)';
             SELECT matches_mark(1) AS result;"
        ),
        Some(1)
    );
    assert_eq!(
        integer_result(
            "CREATE TABLE marks (x INTEGER PRIMARY KEY);
             INSERT INTO marks VALUES (9);
             CREATE FUNCTION matches_mark(x INT) RETURNS BOOLEAN LANGUAGE sql
                 AS 'SELECT EXISTS (SELECT 1 FROM marks WHERE x = $1)';
             SELECT matches_mark(1) AS result;"
        ),
        Some(0)
    );
}

#[test]
fn caller_columns_and_aliases_are_not_captured_by_body_relations() {
    assert_eq!(
        integer_result(
            "CREATE TABLE outer_rows (id INTEGER PRIMARY KEY, owner_id INT);
             CREATE TABLE grants (owner_id INTEGER PRIMARY KEY);
             INSERT INTO outer_rows VALUES (1, 7);
             INSERT INTO grants VALUES (9);
             CREATE FUNCTION has_grant(o INT) RETURNS BOOLEAN LANGUAGE sql
                 AS 'SELECT EXISTS (SELECT 1 FROM grants s WHERE s.owner_id = o)';
             SELECT has_grant(owner_id) AS result FROM outer_rows;"
        ),
        Some(0)
    );
    assert_eq!(
        integer_result(
            "CREATE TABLE outer_rows (id INTEGER PRIMARY KEY, owner_id INT);
             CREATE TABLE grants (owner_id INTEGER PRIMARY KEY);
             INSERT INTO outer_rows VALUES (1, 7);
             INSERT INTO grants VALUES (9);
             CREATE FUNCTION has_grant(o INT) RETURNS BOOLEAN LANGUAGE sql
                 AS 'SELECT EXISTS (SELECT 1 FROM grants s WHERE s.owner_id = o)';
             SELECT has_grant(s.owner_id) AS result FROM outer_rows s;"
        ),
        Some(0)
    );
    assert_eq!(
        integer_result(
            "CREATE TABLE outer_rows (id INTEGER PRIMARY KEY, owner_id INT);
             CREATE TABLE grants (owner_id INTEGER PRIMARY KEY);
             INSERT INTO outer_rows VALUES (1, 7);
             INSERT INTO grants VALUES (9);
             CREATE FUNCTION has_grant(o INT) RETURNS BOOLEAN LANGUAGE sql
                 AS 'SELECT EXISTS (SELECT 1 FROM grants s WHERE s.owner_id = o)';
             SELECT has_grant(owner_id) AS result FROM outer_rows q;"
        ),
        Some(0)
    );
}

#[test]
fn helper_used_in_write_checks_binds_the_new_row() {
    let sql = SHARE_SCHEMA.replace(
        "CREATE POLICY orders_user_share ON orders FOR SELECT USING (shared_with_user(owner_id));",
        "CREATE POLICY orders_user_share ON orders FOR ALL
             USING (shared_with_user(owner_id)) WITH CHECK (shared_with_user(owner_id));",
    );
    let mut connection = share_connection(&sql, "alice");
    use schema::orders;
    diesel::insert_into(orders::table)
        .values((orders::id.eq("shared-insert"), orders::owner_id.eq("bob")))
        .execute(&mut connection)
        .expect("insert shared order");
    assert_eq!(order_ids(&mut connection), ["alice-order", "bob-order", "shared-insert"]);
    let denied = diesel::insert_into(orders::table)
        .values((orders::id.eq("denied"), orders::owner_id.eq("eve")))
        .execute(&mut connection);
    assert!(matches!(denied, Err(diesel::result::Error::DatabaseError(..))));
    let denied = diesel::update(orders::table.find("shared-insert"))
        .set(orders::owner_id.eq("eve"))
        .execute(&mut connection);
    assert!(matches!(denied, Err(diesel::result::Error::DatabaseError(..))));
    assert_eq!(order_ids(&mut connection), ["alice-order", "bob-order", "shared-insert"]);
}

#[test]
fn unsupported_bodies_and_recursive_calls_are_refused() {
    for sql in [
        "CREATE FUNCTION f() RETURNS INT LANGUAGE sql AS 'SELECT 1; SELECT 2'; SELECT f();",
        "CREATE TABLE t(x INT);
         CREATE FUNCTION f() RETURNS INT LANGUAGE sql AS 'SELECT x FROM t'; SELECT f();",
        "CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x'; SELECT f();",
        "CREATE FUNCTION f() RETURNS INT LANGUAGE plpgsql AS 'BEGIN RETURN 1; END'; SELECT f();",
        "CREATE FUNCTION f() RETURNS INT LANGUAGE sql AS 'SELECT f()'; SELECT f();",
        "CREATE FUNCTION f() RETURNS INT LANGUAGE sql AS 'SELECT g()';
         CREATE FUNCTION g() RETURNS INT LANGUAGE sql AS 'SELECT f()'; SELECT f();",
    ] {
        let error = Pg2Sqlite::default()
            .sql(sql)
            .unwrap()
            .translate(&Pg2SqliteOptions::default())
            .unwrap_err();
        assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)), "{error}");
    }
}

#[test]
fn invoker_helpers_cannot_hide_recursive_policy_reads() {
    let sql = "
        CREATE TABLE docs (id INTEGER PRIMARY KEY);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE FUNCTION visible(o INT) RETURNS BOOLEAN LANGUAGE sql
            AS 'SELECT EXISTS (SELECT 1 FROM docs d WHERE d.id = o)';
        CREATE POLICY read_docs ON docs FOR SELECT USING (visible(id));
    ";
    let error = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default().with_rls_audit_table_name("audit"))
        .unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
}

#[test]
fn session_settings_inside_helpers_require_a_mapping() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id TEXT);
             ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
             CREATE FUNCTION belongs(o TEXT) RETURNS BOOLEAN LANGUAGE sql
                 AS 'SELECT o = current_setting(''app.user_id'', true)';
             CREATE POLICY read_docs ON docs FOR SELECT USING (belongs(owner_id));",
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default().with_rls_audit_table_name("audit"))
        .unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::SessionVariableMappingNotFound { .. }));
}

#[test]
fn volatile_arguments_are_refused_when_inlining_cannot_bind_one_evaluation() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE FUNCTION twice(x DOUBLE PRECISION) RETURNS DOUBLE PRECISION LANGUAGE sql
                AS 'SELECT x + x';
             SELECT twice(random());",
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
}

#[test]
fn definer_reads_require_owner_or_declared_bypass_privileges() {
    let owned = format!(
        "{SHARE_SCHEMA}
         CREATE ROLE app_owner;
         ALTER TABLE shares OWNER TO app_owner;
         ALTER FUNCTION shared_with_user(TEXT) OWNER TO app_owner;"
    );
    let mut connection = share_connection(&owned, "alice");
    assert_eq!(order_ids(&mut connection), ["alice-order", "bob-order"]);

    let non_owner = format!(
        "{SHARE_SCHEMA}
         CREATE ROLE app_owner;
         CREATE ROLE reader;
         ALTER TABLE shares OWNER TO app_owner;
         ALTER FUNCTION shared_with_user(TEXT) OWNER TO reader;"
    );
    let error =
        Pg2Sqlite::default().sql(&non_owner).unwrap().translate(&share_options()).unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));

    let bypass = non_owner.replace("CREATE ROLE reader;", "CREATE ROLE reader BYPASSRLS;");
    let mut connection = share_connection(&bypass, "alice");
    assert_eq!(order_ids(&mut connection), ["alice-order", "bob-order"]);
}

#[test]
fn forced_rls_refuses_owner_bypass_but_preserves_superuser_exemption() {
    let forced = format!("{SHARE_SCHEMA} ALTER TABLE shares FORCE ROW LEVEL SECURITY;");
    let error = Pg2Sqlite::default().sql(&forced).unwrap().translate(&share_options()).unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));

    let superuser = format!(
        "{forced}
         CREATE ROLE admin SUPERUSER;
         ALTER FUNCTION shared_with_user(TEXT) OWNER TO admin;"
    );
    let mut connection = share_connection(&superuser, "alice");
    assert_eq!(order_ids(&mut connection), ["alice-order", "bob-order"]);
}

diesel::define_sql_function! {
    /// Returns a value supplied by the destination.
    fn registered_value() -> diesel::sql_types::BigInt;
}

#[test]
fn explicitly_registered_functions_use_the_destination_implementation() {
    let statements = Pg2Sqlite::default()
        .sql(
            "CREATE FUNCTION registered_value() RETURNS BIGINT LANGUAGE sql AS 'SELECT 99';
             SELECT registered_value() AS result;",
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default().with_user_defined_functions(["registered_value"]))
        .unwrap();
    let query = statements
        .iter()
        .find(|statement| matches!(statement, sqlparser::ast::Statement::Query(_)))
        .unwrap();
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    registered_value_utils::register_impl(&mut connection, || 42_i64).unwrap();
    // The translated call must use the registered destination implementation.
    let result =
        diesel::sql_query(query.to_string()).get_result::<IntegerResult>(&mut connection).unwrap();
    assert_eq!(result.result, Some(42));
}

#[test]
fn strict_functions_return_null_without_running_a_constant_body() {
    assert_eq!(
        integer_result(
            "CREATE FUNCTION one(x INT) RETURNS INT LANGUAGE sql STRICT AS 'SELECT 1';
             SELECT one(NULL) AS result;"
        ),
        None
    );
    assert_eq!(
        integer_result(
            "CREATE FUNCTION one(x INT) RETURNS INT LANGUAGE sql STRICT AS 'SELECT 1';
             SELECT one(0) AS result;"
        ),
        Some(1)
    );
}

fn policy_connection(sql: &str, options: &Pg2SqliteOptions) -> SqliteConnection {
    let statements = Pg2Sqlite::default().sql(sql).unwrap().translate(options).unwrap();
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    app_user_utils::register_impl(&mut connection, || "alice".to_owned()).unwrap();
    replica_write_utils::register_impl(&mut connection, || true).unwrap();
    for statement in statements {
        diesel::sql_query(statement.to_string()).execute(&mut connection).unwrap();
    }
    replica_write_utils::register_impl(&mut connection, || false).unwrap();
    connection
}

#[test]
fn helper_inside_policy_subquery_reads_the_inner_column() {
    let sql = "CREATE TABLE docs(id INT PRIMARY KEY);
        CREATE TABLE grants(id INT PRIMARY KEY);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE FUNCTION positive(x INT) RETURNS BOOLEAN LANGUAGE sql AS 'SELECT x > 0';
        CREATE POLICY p ON docs FOR SELECT
            USING (EXISTS (SELECT 1 FROM grants g WHERE positive(id)));
        INSERT INTO docs VALUES (1);
        INSERT INTO grants VALUES (-1);";
    let mut connection = policy_connection(sql, &share_options());
    assert_eq!(schema::docs::table.count().get_result::<i64>(&mut connection).unwrap(), 0);
    diesel::sql_query("UPDATE grants SET id = 1").execute(&mut connection).unwrap();
    assert_eq!(schema::docs::table.count().get_result::<i64>(&mut connection).unwrap(), 1);
}

#[test]
fn derived_and_cte_caller_columns_use_the_exposed_binding() {
    let setup = "CREATE TABLE t(id INT PRIMARY KEY);
        INSERT INTO t VALUES (7);
        CREATE FUNCTION inc(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x + 1';";
    for query in [
        "SELECT inc(id) AS result FROM (SELECT id FROM t) q;",
        "WITH q AS (SELECT id FROM t) SELECT inc(id) AS result FROM q;",
        "SELECT inc(renamed) AS result FROM (SELECT id AS renamed FROM t) q;",
        "WITH q(renamed) AS (SELECT id FROM t) SELECT inc(renamed) AS result FROM q;",
        "SELECT inc(id) AS result FROM (SELECT id + 1 AS id FROM t) q;",
    ] {
        let expected = if query.contains("id + 1") { 9 } else { 8 };
        assert_eq!(integer_result(&format!("{setup} {query}")), Some(expected));
    }
}

#[test]
fn helper_cte_references_bind_the_current_and_new_policy_rows() {
    let sql = "CREATE TABLE docs(id INT PRIMARY KEY);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE FUNCTION positive(o INT) RETURNS BOOLEAN LANGUAGE sql AS
            'SELECT EXISTS (WITH c AS (SELECT o AS v) SELECT 1 FROM c WHERE v > 0)';
        CREATE POLICY p ON docs FOR ALL USING (positive(id)) WITH CHECK (positive(id));
        INSERT INTO docs VALUES (1), (-1);";
    let mut connection = policy_connection(sql, &share_options());
    use schema::docs;
    assert_eq!(docs::table.select(docs::id).load::<i32>(&mut connection).unwrap(), [1]);
    diesel::insert_into(docs::table).values(docs::id.eq(2)).execute(&mut connection).unwrap();
    assert!(
        diesel::insert_into(docs::table).values(docs::id.eq(-2)).execute(&mut connection).is_err()
    );
    diesel::update(docs::table.find(2)).set(docs::id.eq(3)).execute(&mut connection).unwrap();
    assert!(
        diesel::update(docs::table.find(3)).set(docs::id.eq(-3)).execute(&mut connection).is_err()
    );
    assert_eq!(
        docs::table.select(docs::id).order(docs::id).load::<i32>(&mut connection).unwrap(),
        [1, 3]
    );
}

#[test]
fn definer_current_user_reads_the_declared_owner() {
    let sql = "CREATE TABLE docs(id INT PRIMARY KEY);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE ROLE admin;
        CREATE FUNCTION is_admin() RETURNS BOOLEAN LANGUAGE sql SECURITY DEFINER
            AS 'SELECT current_user = ''admin''';
        ALTER FUNCTION is_admin() OWNER TO admin;
        CREATE POLICY p ON docs FOR SELECT USING (is_admin());
        INSERT INTO docs VALUES (1);";
    let options =
        share_options().with_session_variable(SessionVariableMapping::current_user("app_user"));
    let mut connection = policy_connection(sql, &options);
    assert_eq!(schema::docs::table.count().get_result::<i64>(&mut connection).unwrap(), 1);
    let invoker = sql.replace("SECURITY DEFINER", "SECURITY INVOKER");
    let mut connection = policy_connection(&invoker, &options);
    assert_eq!(schema::docs::table.count().get_result::<i64>(&mut connection).unwrap(), 0);
}

#[test]
fn quoted_parameter_references_preserve_identifier_identity() {
    assert_eq!(
        integer_result(
            r#"CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql AS $$SELECT "x" + 1$$;
            SELECT f(4) AS result;"#,
        ),
        Some(5)
    );
    assert_eq!(
        integer_result(
            r#"CREATE FUNCTION f("X" INT) RETURNS INT LANGUAGE sql AS $$SELECT "X" + 1$$;
            SELECT f(4) AS result;"#,
        ),
        Some(5)
    );
}

#[test]
fn bare_caller_argument_survives_a_body_alias_collision() {
    assert_eq!(
        integer_result(
            "CREATE TABLE outer_rows(id INT PRIMARY KEY);
            CREATE TABLE grants(id INT PRIMARY KEY);
            INSERT INTO outer_rows VALUES (7);
            INSERT INTO grants VALUES (9);
            CREATE FUNCTION has_grant(o INT) RETURNS BOOLEAN LANGUAGE sql AS
                'SELECT EXISTS (SELECT 1 FROM grants q WHERE q.id = o)';
            SELECT has_grant(id) AS result FROM outer_rows q;",
        ),
        Some(0)
    );
}

#[test]
fn unaliased_definer_reads_keep_the_original_qualified_binding() {
    let sql = SHARE_SCHEMA.replace("FROM shares s WHERE s.owner_id = o", "FROM shares WHERE shares.owner_id = o")
        .replace("s.grantee = current_setting", "shares.grantee = current_setting")
        .replace("CREATE POLICY shares_grantee ON shares FOR SELECT\n    USING (grantee = current_setting('app.user_id', true));", "");
    let mut connection = share_connection(&sql, "alice");
    assert_eq!(schema::shares::table.count().get_result::<i64>(&mut connection).unwrap(), 0);
    assert_eq!(order_ids(&mut connection), ["alice-order", "bob-order"]);
}

#[test]
fn qualified_recursive_helpers_are_refused() {
    const CHILD: &str = "PG2SQLITE_RECURSION_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let error = Pg2Sqlite::default()
            .sql("CREATE FUNCTION public.f() RETURNS INT LANGUAGE sql AS 'SELECT f()'; SELECT public.f();")
            .unwrap().translate(&Pg2SqliteOptions::default()).unwrap_err();
        assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "qualified_recursive_helpers_are_refused"])
        .env(CHILD, "SQLINLINE-FIX-2")
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "qualified recursion must be refused without aborting");
            break;
        }
        if start.elapsed() >= std::time::Duration::from_secs(5) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("SQLINLINE-FIX-2 recursive translation exceeded five seconds");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn function_search_path_resolves_nested_helpers_under_the_declared_path() {
    let schema = Pg2Sqlite::default()
        .sql(
            "CREATE SCHEMA app;
             CREATE FUNCTION public.g() RETURNS INT LANGUAGE sql AS 'SELECT 1';
             CREATE FUNCTION app.g() RETURNS INT LANGUAGE sql AS 'SELECT 2';
             CREATE FUNCTION public.f() RETURNS INT LANGUAGE sql
                 SET search_path TO public, pg_catalog, pg_temp AS 'SELECT g()';
             SET search_path TO app, public;",
        )
        .unwrap()
        .build_schema()
        .unwrap();
    let statements = Pg2Sqlite::default()
        .sql("SELECT public.f() AS result;")
        .unwrap()
        .translate_with_schema(&schema, &Pg2SqliteOptions::default())
        .unwrap();
    let query = statements
        .iter()
        .find(|statement| matches!(statement, sqlparser::ast::Statement::Query(_)))
        .unwrap();
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    // The translated query exercises the retained catalog search path.
    let row =
        diesel::sql_query(query.to_string()).get_result::<IntegerResult>(&mut connection).unwrap();
    assert_eq!(row.result, Some(1));
}

#[test]
fn nested_helpers_read_the_body_relation_column() {
    assert_eq!(
        integer_result(
            "CREATE TABLE outer_rows(id INT PRIMARY KEY);
            CREATE TABLE marks(id INT PRIMARY KEY);
            INSERT INTO outer_rows VALUES (1);
            INSERT INTO marks VALUES (9);
            CREATE FUNCTION is_nine(x INT) RETURNS BOOLEAN LANGUAGE sql AS 'SELECT x = 9';
            CREATE FUNCTION has_nine() RETURNS BOOLEAN LANGUAGE sql AS
                'SELECT EXISTS (SELECT 1 FROM marks WHERE is_nine(id))';
            SELECT has_nine() AS result FROM outer_rows;",
        ),
        Some(1)
    );
}

#[test]
fn cte_columns_shadow_parameters_and_stored_tables() {
    for setup in ["", "CREATE TABLE c(x INT PRIMARY KEY); INSERT INTO c VALUES (1);"] {
        assert_eq!(
            integer_result(&format!(
                "{setup} CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql AS
                'SELECT (WITH c AS (SELECT 9 AS x) SELECT x FROM c)';
                SELECT f(1) AS result;"
            )),
            Some(9)
        );
    }
    assert_eq!(
        integer_result(
            "CREATE TABLE outer_rows(id INT PRIMARY KEY);
            INSERT INTO outer_rows VALUES (1);
            CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql AS
                'SELECT (WITH q AS (SELECT 9 AS y) SELECT x FROM q)';
            SELECT f(q.id) AS result FROM outer_rows q;",
        ),
        Some(1)
    );
}

#[test]
fn root_aggregate_and_window_bodies_do_not_escape_into_the_caller() {
    for body in ["SELECT count(*)", "SELECT row_number() OVER ()"] {
        let error = Pg2Sqlite::default()
            .sql(&format!(
                "CREATE FUNCTION one() RETURNS BIGINT LANGUAGE sql AS '{body}'; SELECT one();"
            ))
            .unwrap()
            .translate(&Pg2SqliteOptions::default())
            .unwrap_err();
        assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
    }
    assert_eq!(integer_result(
        "CREATE TABLE t(id INT PRIMARY KEY);
            INSERT INTO t VALUES (1), (2);
            CREATE FUNCTION total() RETURNS BIGINT LANGUAGE sql AS 'SELECT (SELECT count(*) FROM t)';
            SELECT total() AS result;",
    ), Some(2));
}

#[test]
fn fetch_bound_positional_arguments_expand_in_helper_subqueries() {
    assert_eq!(
        integer_result(
            "CREATE TABLE t(id INT PRIMARY KEY);
            INSERT INTO t VALUES (1);
            CREATE FUNCTION any_rows(n INT) RETURNS BOOLEAN LANGUAGE sql AS
                'SELECT EXISTS (SELECT 1 FROM t FETCH FIRST $1 ROWS ONLY)';
            SELECT any_rows(0) AS result;",
        ),
        Some(0)
    );
    assert_eq!(
        integer_result(
            "CREATE TABLE t(id INT PRIMARY KEY);
            INSERT INTO t VALUES (1);
            CREATE FUNCTION any_rows(n INT) RETURNS BOOLEAN LANGUAGE sql AS
                'SELECT EXISTS (SELECT 1 FROM t FETCH FIRST $1 ROWS ONLY)';
            SELECT any_rows(1) AS result;",
        ),
        Some(1)
    );
}

#[test]
fn nested_invoker_current_user_inherits_the_definer_identity() {
    let options = Pg2SqliteOptions::default()
        .with_session_variable(SessionVariableMapping::current_user("app_user"))
        .with_user_defined_functions(["app_user"]);
    for expression in ["current_user", "who_am_i()"] {
        assert_eq!(integer_result_with_options(&format!(
            "CREATE ROLE app_owner;
                CREATE FUNCTION who_am_i() RETURNS TEXT LANGUAGE sql SECURITY INVOKER AS 'SELECT current_user';
                CREATE FUNCTION is_owner(o TEXT) RETURNS BOOLEAN LANGUAGE sql SECURITY DEFINER AS 'SELECT o = {expression}';
                ALTER FUNCTION is_owner(TEXT) OWNER TO app_owner;
                SELECT is_owner('bob') AS result;"
        ), &options), Some(0));
    }
    let error = Pg2Sqlite::default()
        .sql("CREATE FUNCTION who_am_i() RETURNS TEXT LANGUAGE sql SECURITY DEFINER AS 'SELECT current_user'; SELECT who_am_i();")
        .unwrap().translate(&options).unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
}

#[test]
fn distinct_on_parameters_expand_within_their_own_subquery() {
    assert_eq!(
        integer_result(
            "CREATE TABLE t(id INT PRIMARY KEY);
             INSERT INTO t VALUES (1), (2), (3);
             CREATE FUNCTION bucket_sum(x INT) RETURNS INT LANGUAGE sql AS
                 'SELECT (SELECT SUM(id) FROM (SELECT DISTINCT ON (id % $1) id FROM t ORDER BY id % $1, id) q)';
             SELECT bucket_sum(2) AS result;",
        ),
        Some(3)
    );
}

#[test]
fn function_qualified_parameters_remain_distinct_from_local_columns() {
    assert_eq!(
        integer_result(
            "CREATE TABLE marks(x INT PRIMARY KEY);
             INSERT INTO marks VALUES (9);
             CREATE FUNCTION matches_mark(x INT) RETURNS BOOLEAN LANGUAGE sql AS
                 'SELECT EXISTS (SELECT 1 FROM marks WHERE x = matches_mark.x)';
             SELECT matches_mark(1) AS result;",
        ),
        Some(0)
    );
    assert_eq!(
        integer_result(
            "CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql AS 'SELECT f.x + 1';
             SELECT f(4) AS result;",
        ),
        Some(5)
    );
    assert_eq!(
        integer_result(
            "CREATE TABLE marks(x INT PRIMARY KEY);
             INSERT INTO marks VALUES (9);
             CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql AS
                 'SELECT (SELECT f.x FROM marks f)';
             SELECT f(1) AS result;",
        ),
        Some(9)
    );
}

#[test]
fn registered_functions_keep_qualified_name_refusal() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE FUNCTION public.registered_value() RETURNS BIGINT LANGUAGE sql AS 'SELECT 99';
             SELECT public.registered_value();",
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default().with_user_defined_functions(["registered_value"]))
        .unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
}

#[test]
fn quoted_alias_hygiene_preserves_local_qualified_columns() {
    assert_eq!(
        integer_result(
            r#"CREATE TABLE outer_rows(owner_id INT PRIMARY KEY);
               CREATE TABLE grants(owner_id INT PRIMARY KEY);
               INSERT INTO outer_rows VALUES (7);
               INSERT INTO grants VALUES (9);
               CREATE FUNCTION "X"(owner_id INT) RETURNS BOOLEAN LANGUAGE sql AS
                   'SELECT EXISTS (SELECT 1 FROM grants "X" WHERE "X".owner_id = $1)';
               SELECT "X"("X".owner_id) AS result FROM outer_rows "X";"#,
        ),
        Some(0)
    );
}

#[test]
fn catalog_scalar_aggregate_names_preserve_caller_aggregation() {
    assert_eq!(
        integer_result(
            "CREATE TABLE t(id INT PRIMARY KEY);
             INSERT INTO t VALUES (1), (2);
             CREATE FUNCTION public.max(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x + 1';
             CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql
                 SET search_path TO public, pg_catalog, pg_temp AS 'SELECT public.max(x)';
             SELECT f(SUM(id)) AS result FROM t;",
        ),
        Some(4)
    );
}

#[test]
fn output_aliases_shadow_parameters_in_ordering_clauses() {
    for order in ["ORDER BY x LIMIT 1", "ORDER BY (x) LIMIT 1"] {
        assert_eq!(
            integer_result(&format!(
                "CREATE TABLE t(id INT PRIMARY KEY, y INT);
                 INSERT INTO t VALUES (1, 4), (2, 1);
                 CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql
                     AS 'SELECT (SELECT y AS x FROM t {order})';
                 SELECT f(-1) AS result;"
            )),
            Some(1)
        );
    }
}

#[test]
fn parameter_projections_keep_implicit_cte_column_names() {
    assert_eq!(
        integer_result(
            "CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql
                 AS 'SELECT (WITH c AS (SELECT x) SELECT c.x FROM c)';
             SELECT f(7) AS result;",
        ),
        Some(7)
    );
}

#[test]
fn implicit_catalog_functions_precede_public_declarations() {
    assert_eq!(
        integer_result(
            "CREATE FUNCTION public.abs(x INT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
             SELECT ABS(-1) AS result;",
        ),
        Some(1)
    );
}

#[test]
fn definer_role_keywords_use_the_effective_owner() {
    assert_eq!(
        integer_result(
            "CREATE ROLE helper_owner;
             CREATE FUNCTION f() RETURNS BOOLEAN LANGUAGE sql SECURITY DEFINER AS
                 'SELECT CURRENT_USER = ''helper_owner''
                     AND CURRENT_ROLE = ''helper_owner'' AND USER = ''helper_owner''';
             ALTER FUNCTION f() OWNER TO helper_owner;
             SELECT f() AS result;",
        ),
        Some(1)
    );
}

#[test]
fn output_labels_shadow_parameters_in_grouping_clauses() {
    for group in ["GROUP BY x", "GROUP BY (x)"] {
        assert_eq!(
            integer_result(&format!(
                "CREATE TABLE t(id INT PRIMARY KEY, y INT);
                 INSERT INTO t VALUES (1, 4), (2, 1);
                 CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql AS
                     'SELECT (SELECT COUNT(*) FROM (SELECT y AS x FROM t {group}) q)';
                 SELECT f(-1) AS result;"
            )),
            Some(2)
        );
    }
}

#[test]
fn implicit_function_labels_win_in_ordering_over_shadowing_parameters() {
    assert_eq!(
        integer_result(
            "CREATE TABLE t(id INT PRIMARY KEY, y INT);
             INSERT INTO t VALUES (1, 4), (2, 1);
             CREATE FUNCTION h(abs INT) RETURNS INT LANGUAGE sql AS
                 'SELECT (SELECT abs(y) FROM t ORDER BY abs LIMIT 1)';
             SELECT h(-1) AS result;"
        ),
        Some(1)
    );
}

#[test]
fn inlined_projections_keep_the_implicit_output_label_for_consumers() {
    for body in [
        "SELECT (WITH c AS (SELECT plus_one(3)) SELECT c.plus_one FROM c)",
        "SELECT (SELECT q.plus_one FROM (SELECT plus_one(3)) q)",
    ] {
        assert_eq!(
            integer_result(&format!(
                "CREATE FUNCTION plus_one(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x + 1';
                 CREATE FUNCTION f() RETURNS INT LANGUAGE sql AS '{body}';
                 SELECT f() AS result;"
            )),
            Some(4)
        );
    }
}

#[test]
fn inherited_search_path_orders_public_and_catalog_priority() {
    for (path, expected) in [
        ("public, pg_catalog", 99),
        ("pg_catalog, public", 1),
        ("public, PG_CATALOG", 99),
        ("PG_CATALOG, public", 1),
    ] {
        let schema = Pg2Sqlite::default()
            .sql(&format!(
                "CREATE FUNCTION public.abs(x INT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
                 CREATE FUNCTION f() RETURNS INT LANGUAGE sql AS 'SELECT abs(-1)';
                 SET search_path TO {path};"
            ))
            .unwrap()
            .build_schema()
            .unwrap();
        let statements = Pg2Sqlite::default()
            .sql("SELECT f() AS result;")
            .unwrap()
            .translate_with_schema(&schema, &Pg2SqliteOptions::default())
            .unwrap();
        let query = statements
            .iter()
            .find(|statement| matches!(statement, sqlparser::ast::Statement::Query(_)))
            .unwrap();
        let mut connection = SqliteConnection::establish(":memory:").unwrap();
        // The translated query is the runtime input under test.
        let row = diesel::sql_query(query.to_string())
            .get_result::<IntegerResult>(&mut connection)
            .unwrap();
        assert_eq!(row.result, Some(expected), "search path {path}");
    }
}

#[test]
fn qualified_public_abs_still_resolves_the_user_declaration() {
    assert_eq!(
        integer_result(
            "CREATE FUNCTION public.abs(x INT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
             SELECT public.abs(-1) AS result;"
        ),
        Some(99)
    );
}

#[test]
fn overloaded_public_abs_keeps_the_implicit_native_predicate() {
    const OVERLOADS: &str =
        "CREATE FUNCTION public.abs(x INT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
         CREATE FUNCTION public.abs(x DOUBLE PRECISION) RETURNS INT LANGUAGE sql AS 'SELECT 100';";
    assert_eq!(integer_result(&format!("{OVERLOADS} SELECT ABS(-1) AS result;")), Some(1));
    let mut connection = policy_connection(
        &format!(
            "{OVERLOADS}
             CREATE TABLE docs(id INT PRIMARY KEY);
             ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
             CREATE POLICY p ON docs FOR SELECT USING (ABS(id) = 3);
             INSERT INTO docs VALUES (3);
             INSERT INTO docs VALUES (4);"
        ),
        &Pg2SqliteOptions::default()
            .with_rls_audit_table_name("audit")
            .with_write_exemption_function("replica_write"),
    );
    assert_eq!(schema::docs::table.count().get_result::<i64>(&mut connection).unwrap(), 1);
}

#[test]
fn user_shadow_cannot_replace_the_implicit_native_format() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE FUNCTION public.format(x TEXT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
             SELECT format('abc') AS result;",
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
}

#[derive(QueryableByName)]
struct TextResult {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    result: Option<String>,
}

fn text_result(sql: &str) -> Option<String> {
    let statements = Pg2Sqlite::default()
        .sql(sql)
        .expect("parse expression")
        .translate(&Pg2SqliteOptions::default())
        .expect("inline expression");
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    app_user_utils::register_impl(&mut connection, || "bob".to_owned()).unwrap();
    let mut result = None;
    for statement in statements {
        if matches!(statement, sqlparser::ast::Statement::Query(_)) {
            // The translated expression is the runtime input under test.
            result = diesel::sql_query(statement.to_string())
                .get_result::<TextResult>(&mut connection)
                .expect("execute translated expression")
                .result;
        } else {
            diesel::sql_query(statement.to_string()).execute(&mut connection).unwrap();
        }
    }
    result
}

#[test]
fn user_shadows_cannot_replace_the_implicit_native_json_setters() {
    for name in ["jsonb_insert", "jsonb_set"] {
        assert_eq!(
            text_result(&format!(
                "CREATE FUNCTION public.{name}(x TEXT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
                 SELECT {name}('{{}}', '{{a}}', '2') AS result;"
            )),
            Some("{\"a\":2}".to_owned())
        );
    }
}

#[test]
fn output_labels_do_not_shadow_parameters_inside_ordering_expressions() {
    assert_eq!(
        integer_result(
            "CREATE TABLE t(id INT PRIMARY KEY, y INT);
             INSERT INTO t VALUES (1, 4), (2, 1);
             CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql AS
                 'SELECT (SELECT y AS x FROM t ORDER BY x + id LIMIT 1)';
             SELECT f(-1) AS result;",
        ),
        Some(4)
    );
}

#[test]
fn output_labels_do_not_shadow_parameters_in_having() {
    assert_eq!(
        integer_result(
            "CREATE TABLE t(id INT PRIMARY KEY);
             INSERT INTO t VALUES (1), (2);
             CREATE FUNCTION f(x INT) RETURNS BOOLEAN LANGUAGE sql AS
                 'SELECT EXISTS (SELECT COUNT(*) AS x FROM t HAVING x > 0)';
             SELECT f(0) AS result;",
        ),
        Some(0)
    );
}

#[test]
fn bare_current_role_keeps_keyword_precedence_over_a_quoted_parameter() {
    assert_eq!(
        integer_result(
            r#"CREATE ROLE helper_owner;
               CREATE FUNCTION f("current_role" TEXT) RETURNS BOOLEAN LANGUAGE sql SECURITY DEFINER
                   AS $$SELECT CURRENT_ROLE = 'helper_owner'$$;
               ALTER FUNCTION f(TEXT) OWNER TO helper_owner;
               SELECT f('not-owner') AS result;"#,
        ),
        Some(1)
    );
}

#[test]
fn quoted_function_names_keep_their_case_against_catalog_names() {
    assert_eq!(
        integer_result(
            r#"CREATE FUNCTION "Abs"(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x + 1';
               SELECT "Abs"(4) AS result;"#,
        ),
        Some(5)
    );
    assert_eq!(
        integer_result(
            r#"CREATE FUNCTION "Abs"(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x + 1';
               SELECT Abs(4) AS result;"#,
        ),
        Some(4)
    );
}

#[test]
fn query_reading_arguments_are_refused_when_reads_cannot_be_bound_once() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE t(id INT PRIMARY KEY);
             CREATE TABLE q(id INT PRIMARY KEY);
             CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x + x';
             SELECT f((SELECT id FROM q)) FROM t;",
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
}

#[test]
fn nested_helpers_keep_the_caller_column_binding() {
    assert_eq!(
        integer_result(
            "CREATE TABLE t(id INT PRIMARY KEY, x INT);
             INSERT INTO t VALUES (1, 5);
             CREATE FUNCTION inner_add(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x + 1';
             CREATE FUNCTION outer_add(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x + 1';
             SELECT outer_add(inner_add(x)) AS result FROM t;"
        ),
        Some(7)
    );
}

#[test]
fn qualified_caller_arguments_survive_body_column_collisions() {
    assert_eq!(
        integer_result(
            "CREATE TABLE outer_t(id INT PRIMARY KEY);
             CREATE TABLE body_t(id INT PRIMARY KEY);
             INSERT INTO outer_t VALUES (5);
             INSERT INTO body_t VALUES (9);
             CREATE FUNCTION bump(x INT) RETURNS INT LANGUAGE sql AS
                 'SELECT x + (SELECT id FROM body_t)';
             SELECT bump(id) AS result FROM outer_t;"
        ),
        Some(14)
    );
}

#[test]
fn source_abs_text_overload_binds_the_proven_signature() {
    assert_eq!(
        integer_result(
            "CREATE FUNCTION public.abs(x TEXT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
             SELECT abs('hello'::TEXT) AS result;"
        ),
        Some(99)
    );
    assert_eq!(
        integer_result(
            "CREATE FUNCTION public.abs(x TEXT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
             SELECT abs(-1) AS result;"
        ),
        Some(1)
    );
    for path in ["public, pg_catalog", "pg_catalog, public"] {
        let schema = Pg2Sqlite::default()
            .sql(&format!(
                "CREATE FUNCTION public.abs(x TEXT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
                 SET search_path TO {path};"
            ))
            .unwrap()
            .build_schema()
            .unwrap();
        for (sql, expected) in [
            ("SELECT abs('hello'::TEXT) AS result;", Some(99)),
            ("SELECT abs(-1) AS result;", Some(1)),
        ] {
            let statements = Pg2Sqlite::default()
                .sql(sql)
                .unwrap()
                .translate_with_schema(&schema, &Pg2SqliteOptions::default())
                .unwrap();
            let query = statements
                .iter()
                .find(|statement| matches!(statement, sqlparser::ast::Statement::Query(_)))
                .unwrap();
            let mut connection = SqliteConnection::establish(":memory:").unwrap();
            app_user_utils::register_impl(&mut connection, || "bob".to_owned()).unwrap();
            // The translated query is the runtime input under test.
            let row = diesel::sql_query(query.to_string())
                .get_result::<IntegerResult>(&mut connection)
                .unwrap();
            assert_eq!(row.result, expected, "search path {path}");
        }
    }
}

#[test]
fn proven_scoped_column_and_bound_parameter_select_native_abs() {
    assert_eq!(
        integer_result(
            "CREATE TABLE docs(id INT PRIMARY KEY);
             INSERT INTO docs VALUES (3);
             CREATE FUNCTION public.abs(x TEXT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
             SELECT abs(id) AS result FROM docs;"
        ),
        Some(3)
    );
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE v(t VARCHAR(10) PRIMARY KEY);
             INSERT INTO v VALUES ('a');
             CREATE FUNCTION public.abs(x TEXT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
             SELECT abs(t) AS result FROM v;",
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE FUNCTION public.abs(x TEXT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
             SELECT abs($1) AS result;",
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
    let statements = Pg2Sqlite::default()
        .sql(
            "CREATE FUNCTION public.abs(x TEXT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
             SELECT abs($1::INT) AS result;",
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .expect("the cast proves the native argument class");
    let emitted = statements
        .iter()
        .find(|statement| matches!(statement, sqlparser::ast::Statement::Query(_)))
        .map(sqlparser::ast::Statement::to_string)
        .unwrap();
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    // The translated query consumes an actual bound argument.
    let result = diesel::sql_query(emitted)
        .bind::<diesel::sql_types::Integer, _>(-3)
        .get_result::<IntegerResult>(&mut connection)
        .unwrap();
    assert_eq!(result.result, Some(3));
}

#[test]
fn abs_arity_mismatch_inlines_the_proven_source_or_refuses() {
    assert_eq!(
        integer_result(
            "CREATE FUNCTION public.abs(x INT, y INT) RETURNS INT LANGUAGE sql AS 'SELECT 3';
             SELECT abs(1, 2) AS result;"
        ),
        Some(3)
    );
    for sql in [
        "CREATE FUNCTION public.abs(x TEXT, y TEXT) RETURNS INT LANGUAGE sql AS 'SELECT 3';
         SELECT abs(1, 2) AS result;",
        "SELECT abs(1, 2) AS result;",
        "CREATE FUNCTION public.jsonb_set(x TEXT) RETURNS INT LANGUAGE sql AS 'SELECT 3';
         SELECT jsonb_set('a') AS result;",
    ] {
        let error = Pg2Sqlite::default()
            .sql(sql)
            .unwrap()
            .translate(&Pg2SqliteOptions::default())
            .unwrap_err();
        assert!(
            matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)),
            "expected a refusal for {sql}"
        );
    }
}

#[test]
fn source_shadow_of_an_implicit_native_aggregate_name_is_refused() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE FUNCTION public.max(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x + 1';
             CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql AS 'SELECT max(x)';
             SELECT f(2) AS result;",
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE sql AS 'SELECT max(x)';
             SELECT f(2) AS result;",
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
}

#[test]
fn native_numeric_overloads_keep_exact_argument_types() {
    for (argument, expected) in
        [("-1::BIGINT", 1), ("-1::SMALLINT", 1), ("2147483648", 2_147_483_648), ("-2147483648", 99)]
    {
        assert_eq!(
            integer_result(&format!(
                "CREATE FUNCTION public.abs(x INT) RETURNS BIGINT LANGUAGE sql AS 'SELECT 99';
                 CREATE FUNCTION numeric_width() RETURNS BIGINT LANGUAGE sql
                     SET search_path TO public, pg_catalog, pg_temp AS 'SELECT abs({argument})';
                 SELECT numeric_width() AS result;"
            )),
            Some(expected),
            "{argument}",
        );
    }
    assert_eq!(
        integer_result(
            "CREATE FUNCTION public.abs(x REAL) RETURNS BIGINT LANGUAGE sql AS 'SELECT 99';
             CREATE FUNCTION numeric_width() RETURNS BIGINT LANGUAGE sql
                 SET search_path TO public, pg_catalog, pg_temp AS 'SELECT abs(-1::DOUBLE PRECISION)';
             SELECT numeric_width() AS result;"
        ),
        Some(1)
    );
}

#[test]
fn native_binding_uses_the_aliased_column_declaration() {
    for (alias, column) in [("q", "q.id"), ("q(exposed)", "q.exposed")] {
        assert_eq!(
            integer_result(&format!(
                "CREATE TABLE docs(id BIGINT PRIMARY KEY);
                 CREATE TABLE q(id INT PRIMARY KEY, exposed INT);
                 INSERT INTO docs VALUES(-1);
                 CREATE FUNCTION public.abs(x INT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
                 CREATE FUNCTION scoped_abs() RETURNS INT LANGUAGE sql
                     SET search_path TO public, pg_catalog, pg_temp AS
                     'SELECT (SELECT abs({column}) FROM docs {alias})';
                 SELECT scoped_abs() AS result;"
            )),
            Some(1),
        );
    }
}

#[test]
fn native_binding_refuses_unproven_local_projection_types() {
    for relation in ["(SELECT id FROM docs) q", "c q"] {
        let body = if relation == "c q" {
            format!("WITH c AS (SELECT id FROM docs) SELECT abs(q.id) FROM {relation}")
        } else {
            format!("SELECT abs(q.id) FROM {relation}")
        };
        let error = Pg2Sqlite::default()
            .sql(&format!(
                "CREATE TABLE docs(id BIGINT PRIMARY KEY);
                 CREATE TABLE q(id INT PRIMARY KEY);
                 CREATE FUNCTION public.abs(x INT) RETURNS INT LANGUAGE sql AS 'SELECT 99';
                 CREATE FUNCTION scoped_abs() RETURNS INT LANGUAGE sql
                     SET search_path TO public, pg_catalog, pg_temp AS 'SELECT ({body})';
                 SELECT scoped_abs();"
            ))
            .unwrap()
            .translate(&Pg2SqliteOptions::default())
            .unwrap_err();
        assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
    }
}

#[test]
fn source_functions_follow_applicable_search_path_signatures() {
    for (first, later, call, expected) in [
        (
            "CREATE FUNCTION public.helper(x INT) RETURNS INT LANGUAGE sql AS 'SELECT x';",
            "CREATE FUNCTION app.helper(x INT, y INT) RETURNS INT LANGUAGE sql AS 'SELECT x + y';",
            "helper(1, 2)",
            3,
        ),
        (
            "CREATE FUNCTION public.helper(x INT) RETURNS INT LANGUAGE plpgsql AS 'BEGIN RETURN x; END;';",
            "CREATE FUNCTION app.helper(x INT, y INT) RETURNS INT LANGUAGE sql AS 'SELECT x + y';",
            "helper(1, 2)",
            3,
        ),
        (
            "CREATE FUNCTION public.helper(x TEXT) RETURNS INT LANGUAGE sql AS 'SELECT 99';",
            "CREATE FUNCTION app.helper(x INT) RETURNS INT LANGUAGE sql AS 'SELECT 7';",
            "helper(1)",
            7,
        ),
    ] {
        let schema = Pg2Sqlite::default()
            .sql(&format!("CREATE SCHEMA app; {first} {later} SET search_path TO public, app;"))
            .unwrap()
            .build_schema()
            .unwrap();
        let statements = Pg2Sqlite::default()
            .sql(&format!("SELECT {call} AS result;"))
            .unwrap()
            .translate_with_schema(&schema, &Pg2SqliteOptions::default())
            .unwrap();
        let query = statements
            .iter()
            .find(|statement| matches!(statement, sqlparser::ast::Statement::Query(_)))
            .unwrap();
        let mut connection = SqliteConnection::establish(":memory:").unwrap();
        // The translated call is selected from the retained catalog.
        let row = diesel::sql_query(query.to_string())
            .get_result::<IntegerResult>(&mut connection)
            .unwrap();
        assert_eq!(row.result, Some(expected), "{first}");
    }
}

#[test]
fn source_defaults_participate_in_native_overload_binding() {
    assert_eq!(
        integer_result(
            "CREATE FUNCTION public.abs(x INT, y INT DEFAULT 2) RETURNS INT LANGUAGE sql AS 'SELECT x + y';
             CREATE FUNCTION default_abs() RETURNS INT LANGUAGE sql
                 SET search_path TO public, pg_catalog, pg_temp AS 'SELECT abs(1)';
             SELECT default_abs() AS result;"
        ),
        Some(3),
    );
}

#[test]
fn query_owned_arguments_cannot_move_into_a_body_subquery() {
    for argument in ["COUNT(*)", "ROW_NUMBER() OVER (ORDER BY outer_rows.id)"] {
        let error = Pg2Sqlite::default()
            .sql(&format!(
                "CREATE TABLE outer_rows(id INT PRIMARY KEY);
                 CREATE TABLE inner_rows(id INT PRIMARY KEY);
                 CREATE FUNCTION f(x BIGINT) RETURNS BIGINT LANGUAGE sql AS
                     'SELECT (SELECT x FROM inner_rows LIMIT 1)';
                 SELECT f({argument}) AS result FROM outer_rows;"
            ))
            .unwrap()
            .translate(&Pg2SqliteOptions::default())
            .unwrap_err();
        assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
    }
}

#[test]
fn query_owned_arguments_cannot_disappear_with_an_unused_parameter() {
    for argument in ["COUNT(*)", "ROW_NUMBER() OVER (ORDER BY outer_rows.id)"] {
        let error = Pg2Sqlite::default()
            .sql(&format!(
                "CREATE TABLE outer_rows(id INT PRIMARY KEY);
                 CREATE FUNCTION f(x BIGINT) RETURNS BIGINT LANGUAGE sql AS 'SELECT 1';
                 SELECT f({argument}) AS result FROM outer_rows;"
            ))
            .unwrap()
            .translate(&Pg2SqliteOptions::default())
            .unwrap_err();
        assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
    }
}

#[test]
fn root_substitution_and_strict_checks_retain_caller_query_operations() {
    for (argument, strict, body, expected) in [
        ("COUNT(*)", "", "x + 1", vec![Some(3)]),
        ("COUNT(*)", "STRICT", "1", vec![Some(1)]),
        ("ROW_NUMBER() OVER (ORDER BY outer_rows.id)", "", "x + 1", vec![Some(2), Some(3)]),
        ("ROW_NUMBER() OVER (ORDER BY outer_rows.id)", "STRICT", "1", vec![Some(1), Some(1)]),
    ] {
        let statements = Pg2Sqlite::default()
            .sql(&format!(
                "CREATE TABLE outer_rows(id INT PRIMARY KEY);
                 INSERT INTO outer_rows VALUES(1), (2);
                 CREATE FUNCTION f(x BIGINT) RETURNS BIGINT LANGUAGE sql {strict} AS
                     'SELECT {body}';
                 SELECT f({argument}) AS result FROM outer_rows ORDER BY result;"
            ))
            .unwrap()
            .translate(&Pg2SqliteOptions::default())
            .unwrap();
        let mut connection = SqliteConnection::establish(":memory:").unwrap();
        let mut actual = Vec::new();
        for statement in statements {
            if matches!(statement, sqlparser::ast::Statement::Query(_)) {
                // The translated query determines values and result
                // cardinality.
                actual = diesel::sql_query(statement.to_string())
                    .load::<IntegerResult>(&mut connection)
                    .unwrap()
                    .into_iter()
                    .map(|row| row.result)
                    .collect();
            } else {
                // The translator emits DDL and connection pragmas.
                diesel::sql_query(statement.to_string()).execute(&mut connection).unwrap();
            }
        }
        assert_eq!(actual, expected, "{argument}, {strict}, {body}");
    }
}

#[test]
fn query_owned_arguments_cannot_move_into_cte_values() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE outer_rows(id INT PRIMARY KEY);
             CREATE FUNCTION f(x BIGINT) RETURNS BIGINT LANGUAGE sql AS
                 'SELECT (WITH v(n) AS (VALUES (x)) SELECT n FROM v)';
             SELECT f(COUNT(*)) AS result FROM outer_rows;",
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
}

#[test]
fn query_bodied_helpers_refuse_unproven_write_snapshots() {
    for statement in [
        "UPDATE totals SET val = read_total();",
        "UPDATE totals SET val = val WHERE read_total() > 0;",
        "DELETE FROM totals WHERE read_total() > 0;",
        "INSERT INTO totals VALUES(3, read_total());",
        "INSERT INTO totals SELECT 3, read_total();",
        "INSERT INTO totals VALUES(1, 3) ON CONFLICT(id) DO UPDATE SET val = read_total();",
        "UPDATE totals SET val = val RETURNING read_total() AS result;",
        "DELETE FROM totals WHERE id = 1 RETURNING read_total() AS result;",
        "INSERT INTO totals VALUES(3, 3) RETURNING read_total() AS result;",
        "WITH q AS (SELECT read_total() AS n) UPDATE totals SET val = (SELECT n FROM q);",
        "UPDATE totals SET val = (SELECT read_total());",
        "UPDATE totals SET val = forward_total();",
    ] {
        let error = Pg2Sqlite::default()
            .sql(&format!(
                "CREATE TABLE totals(id INT PRIMARY KEY, val BIGINT);
                 CREATE FUNCTION read_total() RETURNS BIGINT LANGUAGE sql AS
                     'SELECT (SELECT SUM(val) FROM totals)';
                 CREATE FUNCTION forward_total() RETURNS BIGINT LANGUAGE sql AS
                     'SELECT read_total()';
                 {statement}"
            ))
            .unwrap()
            .translate(&Pg2SqliteOptions::default())
            .expect_err(statement);
        assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)), "{statement}");
    }
}

#[test]
fn query_bodied_helpers_refuse_sqlite_definition_restrictions() {
    for definition in [
        "CREATE TABLE default_rows(id INT PRIMARY KEY, val BIGINT DEFAULT read_total());",
        "CREATE TABLE check_rows(id INT PRIMARY KEY, val BIGINT CHECK(val < read_total()));",
    ] {
        let error = Pg2Sqlite::default()
            .sql(&format!(
                "CREATE TABLE totals(id INT PRIMARY KEY, val BIGINT);
                 CREATE FUNCTION read_total() RETURNS BIGINT LANGUAGE sql AS
                     'SELECT (SELECT SUM(val) FROM totals)';
                 {definition}"
            ))
            .unwrap()
            .translate(&Pg2SqliteOptions::default())
            .expect_err(definition);
        assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)), "{definition}");
    }
}

#[test]
fn pure_helpers_keep_write_and_definition_support_without_leaking_context() {
    assert_eq!(
        integer_result(
            "CREATE FUNCTION plus_one(x BIGINT) RETURNS BIGINT LANGUAGE sql AS 'SELECT x + 1';
             CREATE TABLE totals(id INT PRIMARY KEY,
                 val BIGINT DEFAULT plus_one(1) CHECK(val <= plus_one(9)));
             INSERT INTO totals(id) VALUES(1);
             INSERT INTO totals VALUES(2, 2);
             UPDATE totals SET val = plus_one(val);
             CREATE FUNCTION read_total() RETURNS BIGINT LANGUAGE sql AS
                 'SELECT (SELECT SUM(val) FROM totals)';
             SELECT read_total() AS result;"
        ),
        Some(6),
    );
}

#[test]
fn query_bodied_helpers_refuse_index_expression_restrictions() {
    let error = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE totals(id INT PRIMARY KEY, val BIGINT);
             CREATE FUNCTION query_plus_one(x BIGINT) RETURNS BIGINT LANGUAGE sql IMMUTABLE AS
                 'SELECT (SELECT x + 1)';
             CREATE INDEX computed_index ON totals(query_plus_one(val));",
        )
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap_err();
    assert!(matches!(error, pg2sqlite::errors::Error::TranslationRefusal(_)));
}
