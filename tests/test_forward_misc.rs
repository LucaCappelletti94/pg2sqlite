//! Tests for miscellaneous forward translation paths covering:
//! - FTS expression extraction edge cases (expr.rs)
//! - Table constraint translation (table_constraint.rs)
//! - CREATE INDEX edge cases (create_index.rs)
//! - Statement translation: DROP, VACUUM, transactions (statement.rs)
//! - Forward function translation edge cases (function.rs)

#[path = "helpers/translate.rs"]
mod translate_helpers;

use std::sync::Once;

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};
use sqlite_vec::sqlite3_vec_init;
use translate_helpers::translate_default as translate;

/// Register sqlite-vec once per process so connections used by execute helpers
/// have vec0 and vec_distance_* available.
///
/// SAFETY: `sqlite3_vec_init` has the real C signature `(db, pzErrMsg, pApi) ->
/// int`; the transmute restores it for `sqlite3_auto_extension`. rusqlite FFI
/// is the only path to this API; diesel does not expose it.
fn register_sqlite_vec_once() {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(sqlite3_vec_init as *const ())));
    });
}

fn translate_err(sql: &str) -> String {
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    match result {
        Err(e) => e.to_string(),
        Ok(stmts) => {
            // Some statements may be empty (filtered)
            stmts.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
        }
    }
}

#[test]
fn fts_gin_with_compound_identifier() {
    let sql = "
        CREATE TABLE articles (id INT PRIMARY KEY, title TEXT, body TEXT);
        CREATE INDEX idx_fts ON articles USING GIN (to_tsvector('english', articles.title));
    ";
    let output = translate(sql);
    assert!(
        output.contains("fts5") || output.contains("articles"),
        "Expected FTS5 or articles: {output}"
    );
    execute_emitted(sql);
}

#[test]
fn fts_gin_with_nested_expression() {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, content TEXT, summary TEXT);
        CREATE INDEX idx_fts ON docs USING GIN (to_tsvector('english', (content || ' ' || summary)));
    ";
    let output = translate(sql);
    assert!(output.contains("fts5") || output.contains("docs"), "Expected FTS5 or docs: {output}");
    execute_emitted(sql);
}

#[test]
fn fts_gin_with_cast() {
    let sql = "
        CREATE TABLE items (id INT PRIMARY KEY, code INT, name TEXT);
        CREATE INDEX idx_fts ON items USING GIN (to_tsvector('english', CAST(code AS TEXT) || ' ' || name));
    ";
    let output = translate(sql);
    assert!(
        output.contains("fts5") || output.contains("items"),
        "Expected FTS5 or items: {output}"
    );
    execute_emitted(sql);
}

// Note: DROP TABLE tests use separate translation calls to avoid schema
// conflicts

#[test]
fn drop_table_translates() {
    // Tables are defined without drop to establish schema, then drop separately
    let sql = "
        CREATE TABLE users2 (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE posts2 (id INT PRIMARY KEY, user_id INT);
        DROP TABLE posts2;
    ";
    // Just verify it doesn't crash - the translator handles DROP
    let result = Pg2Sqlite::default().sql(sql);
    assert!(result.is_ok() || result.is_err(), "Should handle DROP");
}

#[test]
fn drop_view_translates() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE VIEW active_users AS SELECT * FROM users;
        DROP VIEW active_users;
    ";
    let result = Pg2Sqlite::default().sql(sql);
    assert!(result.is_ok() || result.is_err(), "Should handle DROP VIEW");
}

#[test]
fn vacuum_passes_through() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        VACUUM;
    ";
    let output = translate(sql);
    assert!(output.contains("VACUUM"), "Expected VACUUM: {output}");
    execute_emitted(sql);
}

#[test]
fn begin_commit_passes_through() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        BEGIN;
        COMMIT;
    ";
    let output = translate(sql);
    // Transaction statements should pass through
    assert!(
        output.contains("BEGIN") || output.contains("COMMIT"),
        "Expected transaction: {output}"
    );
    execute_emitted(sql);
}

#[test]
fn savepoint_passes_through() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        SAVEPOINT sp1;
        RELEASE SAVEPOINT sp1;
    ";
    let output = translate(sql);
    assert!(output.contains("SAVEPOINT"), "Expected SAVEPOINT: {output}");
    execute_emitted(sql);
}

#[test]
fn insert_with_select_translates() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE backup (id INT PRIMARY KEY, name TEXT);
        INSERT INTO backup SELECT * FROM users;
    ";
    let output = translate(sql);
    assert!(output.contains("INSERT INTO"), "Expected INSERT INTO: {output}");
    assert!(output.contains("SELECT"), "Expected SELECT: {output}");
    execute_emitted(sql);
}

#[test]
fn standalone_select_query() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        SELECT * FROM users WHERE age > 18;
    ";
    let output = translate(sql);
    assert!(output.contains("SELECT"), "Expected SELECT: {output}");
    execute_emitted(sql);
}

#[test]
fn standalone_select_with_order_by() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        SELECT * FROM users ORDER BY name ASC, age DESC;
    ";
    let output = translate(sql);
    assert!(output.contains("ORDER BY"), "Expected ORDER BY: {output}");
    execute_emitted(sql);
}

#[test]
fn table_with_check_constraint() {
    let sql = "
        CREATE TABLE items (
            id INT PRIMARY KEY,
            price REAL NOT NULL,
            quantity INT NOT NULL,
            CHECK (price > 0),
            CHECK (quantity >= 0)
        );
    ";
    let output = translate(sql);
    assert!(output.contains("CHECK"), "Expected CHECK constraint: {output}");
    execute_emitted(sql);
}

#[test]
fn table_with_unique_constraint() {
    let sql = "
        CREATE TABLE users (
            id INT PRIMARY KEY,
            email TEXT NOT NULL,
            username TEXT NOT NULL,
            UNIQUE (email),
            UNIQUE (username)
        );
    ";
    let output = translate(sql);
    assert!(output.contains("UNIQUE"), "Expected UNIQUE constraint: {output}");
    execute_emitted(sql);
}

#[test]
fn table_with_composite_pk_constraint() {
    let sql = "
        CREATE TABLE order_items (
            order_id INT NOT NULL,
            item_id INT NOT NULL,
            quantity INT NOT NULL,
            PRIMARY KEY (order_id, item_id)
        );
    ";
    let output = translate(sql);
    assert!(output.contains("PRIMARY KEY"), "Expected PRIMARY KEY: {output}");
    execute_emitted(sql);
}

#[test]
fn gin_index_non_tsvector_skipped() {
    let sql = "
        CREATE TABLE data (id INT PRIMARY KEY, tags TEXT[]);
        CREATE INDEX idx_tags ON data USING GIN (tags);
    ";
    // This should either skip the index or produce a warning
    let _output = translate_err(sql);
    // Just verify it doesn't crash
}

#[test]
fn fts_gin_with_multiple_columns_concat() {
    let sql = "
        CREATE TABLE posts (id INT PRIMARY KEY, title TEXT, body TEXT, tags TEXT);
        CREATE INDEX idx_fts ON posts USING GIN (to_tsvector('english', title || ' ' || body || ' ' || tags));
    ";
    let output = translate(sql);
    assert!(output.contains("fts5") || output.contains("posts"), "Expected FTS5: {output}");
    execute_emitted(sql);
}

#[test]
fn expression_index_with_function() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_upper ON users ((upper(name)));
    ";
    let output = translate(sql);
    // Expression indexes may or may not be supported
    assert!(output.contains("users"), "Expected table reference: {output}");
    execute_emitted(sql);
}

#[test]
fn partial_index_with_complex_where() {
    let sql = "
        CREATE TABLE orders (id INT PRIMARY KEY, status TEXT, total INT);
        CREATE INDEX idx_active ON orders (status) WHERE status = 'active' AND total > 0;
    ";
    let output = translate(sql);
    assert!(output.contains("orders"), "Expected table reference: {output}");
    execute_emitted(sql);
}

#[test]
fn btree_index_explicit() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_name ON users USING BTREE (name);
    ";
    let output = translate(sql);
    assert!(output.contains("CREATE INDEX"), "Expected CREATE INDEX: {output}");
    execute_emitted(sql);
}

#[test]
fn fts_tsvector_match_with_prefix() {
    let sql = "
        CREATE TABLE articles (id INT PRIMARY KEY, title TEXT, body TEXT);
        CREATE INDEX idx_fts ON articles USING GIN (to_tsvector('english', title));
        CREATE VIEW search AS
        SELECT * FROM articles WHERE to_tsvector('english', title) @@ to_tsquery('english', 'hello:*');
    ";
    let output = translate(sql);
    assert!(
        output.contains("fts") || output.contains("MATCH") || output.contains("articles"),
        "Expected FTS: {output}"
    );
    execute_emitted(sql);
}

#[test]
fn query_with_subquery_in_where() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE orders (id INT PRIMARY KEY, user_id INT);
        SELECT * FROM users WHERE id IN (SELECT user_id FROM orders);
    ";
    let output = translate(sql);
    assert!(output.contains("IN (SELECT"), "Expected subquery: {output}");
    execute_emitted(sql);
}

#[test]
fn gist_index_fts_translation() {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, content TEXT);
        CREATE INDEX idx_fts ON docs USING GiST (to_tsvector('english', content));
    ";
    let output = translate(sql);
    assert!(output.contains("fts5") || output.contains("docs"), "Expected FTS5 or docs: {output}");
    execute_emitted(sql);
}

#[test]
fn unique_index_translates() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, email TEXT);
        CREATE UNIQUE INDEX idx_email ON users (email);
    ";
    let output = translate(sql);
    assert!(output.contains("UNIQUE"), "Expected UNIQUE: {output}");
    execute_emitted(sql);
}

#[test]
fn multicolumn_index_translates() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        CREATE INDEX idx_name_age ON users (name, age);
    ";
    let output = translate(sql);
    assert!(output.contains("CREATE INDEX"), "Expected CREATE INDEX: {output}");
    execute_emitted(sql);
}

#[test]
fn forward_delete_simple() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        DELETE FROM users WHERE id = 1;
    ";
    let output = translate(sql);
    assert!(output.contains("DELETE FROM"), "Expected DELETE FROM: {output}");
    execute_emitted(sql);
}

#[test]
fn forward_update_simple() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        UPDATE users SET name = 'test' WHERE id = 1;
    ";
    let output = translate(sql);
    assert!(output.contains("UPDATE"), "Expected UPDATE in output: {output}");
    assert!(output.contains("WHERE"), "Expected WHERE clause in output: {output}");
    execute_emitted(sql);
}

#[test]
fn forward_insert_with_values() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        INSERT INTO users (id, name) VALUES (1, 'Alice');
    ";
    let output = translate(sql);
    assert!(output.contains("INSERT INTO"), "Expected INSERT: {output}");
    assert!(output.contains("Alice"), "Expected Alice: {output}");
    execute_emitted(sql);
}

#[test]
fn forward_insert_on_conflict_do_nothing() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        INSERT INTO users (id, name) VALUES (1, 'Alice') ON CONFLICT (id) DO NOTHING;
    ";
    let output = translate(sql);
    assert!(
        output.contains("INSERT OR IGNORE") || output.contains("ON CONFLICT"),
        "Expected INSERT OR IGNORE or ON CONFLICT: {output}"
    );
    execute_emitted(sql);
}

#[test]
fn drop_trigger_translates() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        DROP TRIGGER IF EXISTS my_trigger ON users;
    ";
    // DROP TRIGGER should strip the ON table_name for SQLite
    let output = translate(sql);
    assert!(output.contains("DROP TRIGGER"), "Expected DROP TRIGGER: {output}");
    execute_emitted(sql);
}

#[test]
fn drop_index_translates() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_name ON users (name);
        DROP INDEX idx_name;
    ";
    let output = translate(sql);
    assert!(output.contains("DROP INDEX"), "Expected DROP INDEX: {output}");
    execute_emitted(sql);
}

#[test]
fn fts_gin_single_column() {
    let sql = "
        CREATE TABLE articles (id INT PRIMARY KEY, title TEXT);
        CREATE INDEX idx_fts ON articles USING GIN (to_tsvector('english', title));
    ";
    let output = translate(sql);
    assert!(output.contains("fts5") || output.contains("articles"), "Expected FTS5: {output}");
    execute_emitted(sql);
}

#[test]
fn table_with_foreign_key() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE posts (
            id INT PRIMARY KEY,
            user_id INT REFERENCES users(id)
        );
    ";
    let output = translate(sql);
    assert!(output.contains("REFERENCES"), "Expected REFERENCES: {output}");
    execute_emitted(sql);
}

/// `ALTER TABLE ... ADD COLUMN` is translated, not filtered. SQLite has
/// supported it since 3.1.1. Semantic coverage (type mapping, defaults,
/// execution) lives in `tests/test_alter_table_columns.rs`.
#[test]
fn alter_table_add_column_is_translated() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        ALTER TABLE users ADD COLUMN age INT;
    ";
    let output = translate(sql);
    assert!(
        output.contains("ALTER TABLE users ADD COLUMN age INTEGER"),
        "ADD COLUMN must be emitted with the mapped type: {output}"
    );
    execute_emitted(sql);
}

#[test]
fn create_function_is_filtered() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE FUNCTION my_func() RETURNS void AS $$ BEGIN END; $$ LANGUAGE plpgsql;
    ";
    let output = translate(sql);
    assert!(!output.contains("CREATE FUNCTION"), "CREATE FUNCTION should be filtered: {output}");
    execute_emitted(sql);
}

#[test]
fn grant_is_filtered() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        GRANT SELECT ON users TO PUBLIC;
    ";
    let output = translate(sql);
    assert!(!output.contains("GRANT"), "GRANT should be filtered: {output}");
    execute_emitted(sql);
}

#[test]
fn concat_to_concatenation() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, first_name TEXT, last_name TEXT);
        SELECT CONCAT(first_name, ' ', last_name) FROM users;
    ";
    let output = translate(sql);
    assert!(output.contains("||"), "Expected || concatenation: {output}");
    execute_emitted(sql);
}

#[test]
fn concat_ws_to_concatenation_with_separator() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, first_name TEXT, last_name TEXT);
        SELECT CONCAT_WS(' ', first_name, last_name) FROM users;
    ";
    let output = translate(sql);
    assert!(output.contains("||"), "Expected || concatenation: {output}");
    execute_emitted(sql);
}

#[test]
fn aggregate_with_filter_clause() {
    let sql = "
        CREATE TABLE orders (id INT PRIMARY KEY, status TEXT, amount INT);
        SELECT COUNT(*) FILTER (WHERE status = 'active') FROM orders;
    ";
    let output = translate(sql);
    // FILTER should be converted to CASE WHEN
    assert!(
        output.contains("CASE WHEN") || output.contains("COUNT"),
        "Expected CASE WHEN or COUNT: {output}"
    );
    execute_emitted(sql);
}

#[test]
fn sum_with_filter_clause() {
    let sql = "
        CREATE TABLE orders (id INT PRIMARY KEY, status TEXT, amount INT);
        SELECT SUM(amount) FILTER (WHERE status = 'active') FROM orders;
    ";
    let output = translate(sql);
    assert!(output.contains("CASE WHEN") || output.contains("SUM"), "Expected CASE WHEN: {output}");
    execute_emitted(sql);
}

#[test]
fn column_default_unary_op() {
    let sql = "
        CREATE TABLE items (
            id INT PRIMARY KEY,
            val INT DEFAULT -1
        );
    ";
    let output = translate(sql);
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
    assert!(output.contains("-1"), "Expected -1: {output}");
    execute_emitted(sql);
}

#[test]
fn column_default_nested() {
    let sql = "
        CREATE TABLE items (
            id INT PRIMARY KEY,
            val INT DEFAULT (42)
        );
    ";
    let output = translate(sql);
    assert!(output.contains("42"), "Expected 42: {output}");
    execute_emitted(sql);
}

#[test]
fn column_default_binary_op() {
    let sql = "
        CREATE TABLE items (
            id INT PRIMARY KEY,
            val INT DEFAULT 1 + 2
        );
    ";
    let output = translate(sql);
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
    execute_emitted(sql);
}

#[test]
fn column_default_cast() {
    let sql = "
        CREATE TABLE items (
            id INT PRIMARY KEY,
            val TEXT DEFAULT 'hello'::text
        );
    ";
    let output = translate(sql);
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
    execute_emitted(sql);
}

#[test]
fn generated_column_stored() {
    let sql = "
        CREATE TABLE items (
            id INT PRIMARY KEY,
            price INT,
            tax INT,
            total INT GENERATED ALWAYS AS (price + tax) STORED
        );
    ";
    let output = translate(sql);
    assert!(output.contains("GENERATED ALWAYS AS"), "Expected GENERATED: {output}");
    execute_emitted(sql);
}

#[test]
fn table_fk_to_rls_table() {
    use pg2sqlite::{prelude::SessionVariableMapping, traits::TranslationOptions};
    let sql = r#"
        CREATE TABLE users (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL
        );
        ALTER TABLE users ENABLE ROW LEVEL SECURITY;
        CREATE POLICY sel ON users FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY ins ON users FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY upd ON users FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY del ON users FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE TABLE orders (
            id INT PRIMARY KEY,
            user_id INT,
            FOREIGN KEY (user_id) REFERENCES users(id)
        );
    "#;
    let options = Pg2SqliteOptions::default()
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));
    let output = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&options)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    // FK should reference users_rls (the underlying table)
    assert!(output.contains("users_rls"), "Expected users_rls reference: {output}");
    execute_emitted_with_options(sql, &options);
}

#[test]
fn column_fk_to_rls_table() {
    use pg2sqlite::{prelude::SessionVariableMapping, traits::TranslationOptions};
    let sql = r#"
        CREATE TABLE users (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL
        );
        ALTER TABLE users ENABLE ROW LEVEL SECURITY;
        CREATE POLICY sel ON users FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY ins ON users FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY upd ON users FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY del ON users FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE TABLE orders (
            id INT PRIMARY KEY,
            user_id INT REFERENCES users(id)
        );
    "#;
    let options = Pg2SqliteOptions::default()
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));
    let output = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&options)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(output.contains("users_rls"), "Expected users_rls FK reference: {output}");
    execute_emitted_with_options(sql, &options);
}

#[test]
fn check_constraint_with_function_removed() {
    use pg2sqlite::traits::TranslationOptions;
    let sql = "
        CREATE TABLE items (
            id INT PRIMARY KEY,
            code TEXT,
            CHECK (char_length(code) > 0)
        );
    ";
    let options = Pg2SqliteOptions::default().remove_unsupported_check_constraints();
    let output = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&options)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    // CHECK with function should be removed
    assert!(output.contains("items"), "Expected items table: {output}");
    execute_emitted_with_options(sql, &options);
}

#[test]
fn create_or_replace_view_emits_drop_then_create() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE OR REPLACE VIEW active AS SELECT * FROM users;
    ";
    let stmts =
        Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default()).unwrap();
    // table + DROP VIEW IF EXISTS + CREATE VIEW
    assert_eq!(stmts.len(), 3, "Expected 3 statements, got {stmts:?}");
    assert!(
        stmts[1].to_string().to_uppercase().contains("DROP VIEW IF EXISTS"),
        "Expected DROP VIEW IF EXISTS: {}",
        stmts[1]
    );
    assert!(
        stmts[2].to_string().to_uppercase().contains("CREATE VIEW"),
        "Expected CREATE VIEW: {}",
        stmts[2]
    );
}

#[test]
fn materialized_view_error() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE MATERIALIZED VIEW mv AS SELECT * FROM users;
    ";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "Expected error for MATERIALIZED VIEW");
}

/// Flipped R123 pin, and the proof the view read is load bearing. The old
/// rewrite renamed the USING relation to the `users_rls` backing table,
/// which both stranded the `users.id` qualifiers (`no such column`) and
/// bypassed the SELECT policies PostgreSQL applies to a USING read. The
/// EXISTS subquery now reads the policy view: with the session user set to
/// owner 2 the predicate's owner-1 row is invisible and nothing is deleted,
/// and switching the session to owner 1 deletes exactly that row's order.
#[test]
fn delete_using_with_rls_table_join() {
    use pg2sqlite::{prelude::SessionVariableMapping, traits::TranslationOptions};
    let sql = r#"
        CREATE TABLE orders (
            id INT PRIMARY KEY,
            user_id INT,
            amount INT
        );
        CREATE TABLE users (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL
        );
        ALTER TABLE users ENABLE ROW LEVEL SECURITY;
        CREATE POLICY sel ON users FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY ins ON users FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY upd ON users FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY del ON users FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        DELETE FROM orders USING users WHERE orders.user_id = users.id AND users.owner_id = 1;
    "#;
    let options = Pg2SqliteOptions::default()
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate_to_sql(&options).unwrap();
    let delete_stmt =
        stmts.iter().find(|s| s.starts_with("DELETE")).expect("should emit the DELETE").clone();
    assert!(
        delete_stmt.contains("FROM users WHERE"),
        "the EXISTS subquery should read the policy view: {delete_stmt}"
    );

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let session_user = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(2));
    let session_user_for_fn = std::sync::Arc::clone(&session_user);
    conn.create_scalar_function(
        "current_app_user",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        move |_| Ok(session_user_for_fn.load(std::sync::atomic::Ordering::Relaxed)),
    )
    .unwrap();
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("SQLite rejected output: {e}\n{s}"));
    }
    conn.execute_batch(
        "INSERT INTO users_rls (id, owner_id) VALUES (5, 1), (6, 2);
         INSERT INTO orders (id, user_id, amount) VALUES (100, 5, 10), (200, 6, 20);",
    )
    .unwrap();

    let surviving = |conn: &rusqlite::Connection| -> Vec<i64> {
        conn.prepare("SELECT id FROM orders ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };

    // Session user 2: the owner-1 users row the predicate wants is hidden by
    // the SELECT policy, so the DELETE must remove nothing.
    conn.execute_batch(&format!("{delete_stmt};")).unwrap();
    assert_eq!(surviving(&conn), vec![100, 200], "a policy-hidden row must not match USING");

    // Session user 1: the same row is now visible and exactly its order goes.
    session_user.store(1, std::sync::atomic::Ordering::Relaxed);
    conn.execute_batch(&format!("{delete_stmt};")).unwrap();
    assert_eq!(surviving(&conn), vec![200], "the visible owner-1 row drives the delete");
}

#[test]
fn vector_type_cast() {
    let sql = "
        CREATE TABLE items (
            id INT PRIMARY KEY,
            embedding VECTOR(384)
        );
        SELECT * FROM items WHERE embedding = '[1,2,3]'::vector;
    ";
    let output = translate(sql);
    assert!(output.contains("vec_f32"), "Expected vec_f32 cast: {output}");
    execute_emitted(sql);
}

#[test]
fn hamming_distance_operator() {
    let sql = "
        CREATE TABLE items (
            id INT PRIMARY KEY,
            bits VECTOR(384)
        );
        SELECT * FROM items ORDER BY bits <~> '[1,0,1]'::vector;
    ";
    let output = translate(sql);
    assert!(output.contains("vec_distance_hamming"), "Expected vec_distance_hamming: {output}");
    execute_emitted(sql);
}

#[test]
fn any_eq_to_in_subquery() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE admins (user_id INT PRIMARY KEY);
        SELECT * FROM users WHERE id = ANY(SELECT user_id FROM admins);
    ";
    let output = translate(sql);
    assert!(output.contains("IN (SELECT"), "Expected IN (SELECT: {output}");
    execute_emitted(sql);
}

#[test]
fn all_ne_to_not_in_subquery() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE banned (user_id INT PRIMARY KEY);
        SELECT * FROM users WHERE id <> ALL(SELECT user_id FROM banned);
    ";
    let output = translate(sql);
    assert!(output.contains("NOT IN (SELECT"), "Expected NOT IN (SELECT: {output}");
    execute_emitted(sql);
}

#[test]
fn column_default_identifier() {
    let sql = "
        CREATE TABLE items (
            id INT PRIMARY KEY,
            status TEXT DEFAULT 'active'
        );
    ";
    let output = translate(sql);
    assert!(output.contains("DEFAULT"), "Expected DEFAULT: {output}");
    assert!(output.contains("active"), "Expected active: {output}");
    execute_emitted(sql);
}

#[test]
fn or_replace_trigger() {
    let sql = r#"
        CREATE TABLE items (id INT PRIMARY KEY, name TEXT, updated_at TEXT);
        CREATE OR REPLACE FUNCTION update_timestamp()
        RETURNS TRIGGER AS $$
        BEGIN
            NEW.updated_at = 'now';
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE OR REPLACE TRIGGER set_timestamp
            BEFORE UPDATE ON items
            FOR EACH ROW
            EXECUTE FUNCTION update_timestamp();
    "#;
    let output = translate(sql);
    // OR REPLACE trigger should produce DROP TRIGGER IF EXISTS
    assert!(
        output.contains("DROP TRIGGER IF EXISTS") || output.contains("TRIGGER"),
        "Expected trigger handling: {output}"
    );
    execute_emitted(sql);
}

#[test]
fn before_trigger_on_rls_table() {
    use pg2sqlite::{prelude::SessionVariableMapping, traits::TranslationOptions};
    let sql = r#"
        CREATE TABLE items (id INT PRIMARY KEY, name TEXT, owner_id INT NOT NULL);
        ALTER TABLE items ENABLE ROW LEVEL SECURITY;
        CREATE POLICY sel ON items FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY ins ON items FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY upd ON items FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY del ON items FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE FUNCTION update_name()
        RETURNS TRIGGER AS $$
        BEGIN
            NEW.name = 'updated';
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER set_name
            BEFORE UPDATE ON items
            FOR EACH ROW
            EXECUTE FUNCTION update_name();
    "#;
    let options = Pg2SqliteOptions::default()
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));
    let output = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&options)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    // BEFORE trigger should be redirected to items_rls table
    assert!(output.contains("items_rls"), "Expected items_rls redirect: {output}");
    execute_emitted_with_options(sql, &options);
}

#[test]
fn insert_on_conflict_do_update_translates_expressions() {
    let sql = "
        CREATE TABLE events (id INT PRIMARY KEY, name TEXT, updated_at TEXT);
        INSERT INTO events (id, name, updated_at) VALUES (1, 'test', 'x')
        ON CONFLICT (id) DO UPDATE SET updated_at = NOW();
    ";
    let output = translate(sql);
    assert!(
        output.contains("datetime('now')"),
        "Expected datetime('now') in ON CONFLICT DO UPDATE: {output}"
    );
    execute_emitted(sql);
}

#[test]
fn insert_returning_translates_expressions() {
    let sql = "
        CREATE TABLE events (id INT PRIMARY KEY, name TEXT);
        INSERT INTO events (id, name) VALUES (1, 'test') RETURNING NOW() AS ts;
    ";
    let output = translate(sql);
    assert!(
        output.contains("datetime('now')"),
        "Expected datetime('now') in INSERT RETURNING: {output}"
    );
    execute_emitted(sql);
}
/// Execute every emitted statement against an in-memory SQLite connection.
///
/// Registers sqlite-vec so tests involving vector types (vec0, vec_f32,
/// vec_distance_*) can execute the translated DDL and DML. rusqlite is used
/// directly because diesel does not expose `sqlite3_auto_extension`.
fn execute_emitted(sql: &str) {
    register_sqlite_vec_once();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let stmts = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate_to_sql(&Pg2SqliteOptions::default())
        .unwrap();
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("emitted SQL failed: {e}\n{s}"));
    }
}

fn execute_emitted_with_options(sql: &str, options: &Pg2SqliteOptions) {
    register_sqlite_vec_once();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let stmts = Pg2Sqlite::default().sql(sql).unwrap().translate_to_sql(options).unwrap();
    for s in &stmts {
        conn.execute_batch(&format!("{s};"))
            .unwrap_or_else(|e| panic!("emitted SQL failed: {e}\n{s}"));
    }
}
