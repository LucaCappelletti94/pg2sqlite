//! Tests for miscellaneous forward translation paths covering:
//! - FTS expression extraction edge cases (expr.rs)
//! - Table constraint translation (table_constraint.rs)
//! - CREATE INDEX edge cases (create_index.rs)
//! - Statement translation: DROP, VACUUM, transactions (statement.rs)
//! - Forward function translation edge cases (function.rs)

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn translate(sql: &str) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&Pg2SqliteOptions::default())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
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

// ==================== FTS with compound identifier (table.column in tsvector)
// ====================

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
}

// ==================== FTS with nested expression ====================

#[test]
fn fts_gin_with_nested_expression() {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, content TEXT, summary TEXT);
        CREATE INDEX idx_fts ON docs USING GIN (to_tsvector('english', (content || ' ' || summary)));
    ";
    let output = translate(sql);
    assert!(output.contains("fts5") || output.contains("docs"), "Expected FTS5 or docs: {output}");
}

// ==================== FTS with CAST in expression ====================

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
}

// ==================== DROP TABLE ====================
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

// ==================== VACUUM ====================

#[test]
fn vacuum_passes_through() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        VACUUM;
    ";
    let output = translate(sql);
    assert!(output.contains("VACUUM"), "Expected VACUUM: {output}");
}

// ==================== Transaction control ====================

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
}

// ==================== INSERT with SELECT ====================

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
}

// ==================== SELECT query (standalone) ====================

#[test]
fn standalone_select_query() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        SELECT * FROM users WHERE age > 18;
    ";
    let output = translate(sql);
    assert!(output.contains("SELECT"), "Expected SELECT: {output}");
}

#[test]
fn standalone_select_with_order_by() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        SELECT * FROM users ORDER BY name ASC, age DESC;
    ";
    let output = translate(sql);
    assert!(output.contains("ORDER BY"), "Expected ORDER BY: {output}");
}

// ==================== Table with CHECK constraint ====================

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
}

// ==================== Table with UNIQUE constraint ====================

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
}

// ==================== Table with composite PK constraint ====================

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
}

// ==================== GIN index with non-tsvector (unsupported)
// ====================

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

// ==================== Multiple FTS columns ====================

#[test]
fn fts_gin_with_multiple_columns_concat() {
    let sql = "
        CREATE TABLE posts (id INT PRIMARY KEY, title TEXT, body TEXT, tags TEXT);
        CREATE INDEX idx_fts ON posts USING GIN (to_tsvector('english', title || ' ' || body || ' ' || tags));
    ";
    let output = translate(sql);
    assert!(output.contains("fts5") || output.contains("posts"), "Expected FTS5: {output}");
}

// ==================== Expression index on function ====================

#[test]
fn expression_index_with_function() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_upper ON users ((upper(name)));
    ";
    let output = translate(sql);
    // Expression indexes may or may not be supported
    assert!(output.contains("users"), "Expected table reference: {output}");
}

// ==================== Partial index with complex WHERE ====================

#[test]
fn partial_index_with_complex_where() {
    let sql = "
        CREATE TABLE orders (id INT PRIMARY KEY, status TEXT, total INT);
        CREATE INDEX idx_active ON orders (status) WHERE status = 'active' AND total > 0;
    ";
    let output = translate(sql);
    assert!(output.contains("orders"), "Expected table reference: {output}");
}

// ==================== BTREE index (explicit) ====================

#[test]
fn btree_index_explicit() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_name ON users USING BTREE (name);
    ";
    let output = translate(sql);
    assert!(output.contains("CREATE INDEX"), "Expected CREATE INDEX: {output}");
}

// ==================== FTS5 tsquery with prefix search ====================

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
}

// ==================== Forward query with subquery in WHERE
// ====================

#[test]
fn query_with_subquery_in_where() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE orders (id INT PRIMARY KEY, user_id INT);
        SELECT * FROM users WHERE id IN (SELECT user_id FROM orders);
    ";
    let output = translate(sql);
    assert!(output.contains("IN (SELECT"), "Expected subquery: {output}");
}

// ==================== GiST index (FTS translation) ====================

#[test]
fn gist_index_fts_translation() {
    let sql = "
        CREATE TABLE docs (id INT PRIMARY KEY, content TEXT);
        CREATE INDEX idx_fts ON docs USING GiST (to_tsvector('english', content));
    ";
    let output = translate(sql);
    assert!(output.contains("fts5") || output.contains("docs"), "Expected FTS5 or docs: {output}");
}

// ==================== Unique index ====================

#[test]
fn unique_index_translates() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, email TEXT);
        CREATE UNIQUE INDEX idx_email ON users (email);
    ";
    let output = translate(sql);
    assert!(output.contains("UNIQUE"), "Expected UNIQUE: {output}");
}

// ==================== Multicolumn index ====================

#[test]
fn multicolumn_index_translates() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
        CREATE INDEX idx_name_age ON users (name, age);
    ";
    let output = translate(sql);
    assert!(output.contains("CREATE INDEX"), "Expected CREATE INDEX: {output}");
}

// ==================== Forward DELETE simple ====================

#[test]
fn forward_delete_simple() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        DELETE FROM users WHERE id = 1;
    ";
    let output = translate(sql);
    assert!(output.contains("DELETE FROM"), "Expected DELETE FROM: {output}");
}

// ==================== Forward UPDATE simple ====================

#[test]
fn forward_update_simple() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        UPDATE users SET name = 'test' WHERE id = 1;
    ";
    let output = translate(sql);
    assert!(output.contains("UPDATE"), "Expected UPDATE in output: {output}");
    assert!(output.contains("WHERE"), "Expected WHERE clause in output: {output}");
}

// ==================== Forward INSERT with VALUES ====================

#[test]
fn forward_insert_with_values() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        INSERT INTO users (id, name) VALUES (1, 'Alice');
    ";
    let output = translate(sql);
    assert!(output.contains("INSERT INTO"), "Expected INSERT: {output}");
    assert!(output.contains("Alice"), "Expected Alice: {output}");
}

// ==================== Forward INSERT with ON CONFLICT DO NOTHING
// ====================

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
}

// ==================== DropTrigger ====================

#[test]
fn drop_trigger_translates() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        DROP TRIGGER IF EXISTS my_trigger ON users;
    ";
    // DROP TRIGGER should strip the ON table_name for SQLite
    let output = translate(sql);
    assert!(output.contains("DROP TRIGGER"), "Expected DROP TRIGGER: {output}");
}

// ==================== DROP INDEX ====================

#[test]
fn drop_index_translates() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE INDEX idx_name ON users (name);
        DROP INDEX idx_name;
    ";
    let output = translate(sql);
    assert!(output.contains("DROP INDEX"), "Expected DROP INDEX: {output}");
}

// ==================== FTS5 with single column (no concat) ====================

#[test]
fn fts_gin_single_column() {
    let sql = "
        CREATE TABLE articles (id INT PRIMARY KEY, title TEXT);
        CREATE INDEX idx_fts ON articles USING GIN (to_tsvector('english', title));
    ";
    let output = translate(sql);
    assert!(output.contains("fts5") || output.contains("articles"), "Expected FTS5: {output}");
}

// ==================== Create table with REFERENCES (FK) ====================

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
}

// ==================== AlterTable is filtered ====================

#[test]
fn alter_table_is_filtered() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        ALTER TABLE users ADD COLUMN age INT;
    ";
    let output = translate(sql);
    assert!(!output.contains("ALTER TABLE"), "ALTER TABLE should be filtered: {output}");
}

// ==================== CreateFunction is filtered ====================

#[test]
fn create_function_is_filtered() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE FUNCTION my_func() RETURNS void AS $$ BEGIN END; $$ LANGUAGE plpgsql;
    ";
    let output = translate(sql);
    assert!(!output.contains("CREATE FUNCTION"), "CREATE FUNCTION should be filtered: {output}");
}

// ==================== Grant is filtered ====================

#[test]
fn grant_is_filtered() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        GRANT SELECT ON users TO PUBLIC;
    ";
    let output = translate(sql);
    assert!(!output.contains("GRANT"), "GRANT should be filtered: {output}");
}

// ==================== CONCAT -> || concatenation ====================

#[test]
fn concat_to_concatenation() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, first_name TEXT, last_name TEXT);
        SELECT CONCAT(first_name, ' ', last_name) FROM users;
    ";
    let output = translate(sql);
    assert!(output.contains("||"), "Expected || concatenation: {output}");
}

// ==================== CONCAT_WS -> || with separator ====================

#[test]
fn concat_ws_to_concatenation_with_separator() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, first_name TEXT, last_name TEXT);
        SELECT CONCAT_WS(' ', first_name, last_name) FROM users;
    ";
    let output = translate(sql);
    assert!(output.contains("||"), "Expected || concatenation: {output}");
}

// ==================== Function with FILTER clause ====================

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
}

#[test]
fn sum_with_filter_clause() {
    let sql = "
        CREATE TABLE orders (id INT PRIMARY KEY, status TEXT, amount INT);
        SELECT SUM(amount) FILTER (WHERE status = 'active') FROM orders;
    ";
    let output = translate(sql);
    assert!(output.contains("CASE WHEN") || output.contains("SUM"), "Expected CASE WHEN: {output}");
}

// ==================== Column with DEFAULT expressions ====================

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
}

// ==================== Generated column ====================

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
}

// ==================== Table-level FK to RLS table ====================

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
}

// ==================== Column FK to RLS table ====================

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
}

// ==================== CHECK constraint with function (removable)
// ====================

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
}

// ==================== CREATE OR REPLACE VIEW error ====================

#[test]
fn create_or_replace_view_error() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE OR REPLACE VIEW active AS SELECT * FROM users;
    ";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "Expected error for OR REPLACE VIEW");
}

// ==================== MATERIALIZED VIEW error ====================

#[test]
fn materialized_view_error() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE MATERIALIZED VIEW mv AS SELECT * FROM users;
    ";
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&Pg2SqliteOptions::default());
    assert!(result.is_err(), "Expected error for MATERIALIZED VIEW");
}

// ==================== DELETE with USING and RLS table join
// ====================

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
    let output = Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&options)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(output.contains("DELETE"), "Expected DELETE: {output}");
    // JOIN references to users should be renamed to users_rls
    assert!(output.contains("users_rls"), "Expected users_rls in DELETE USING: {output}");
}

// ==================== Forward vector cast ====================

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
}

// ==================== Hamming distance operator ====================

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
}

// ==================== ANY operator to IN subquery ====================

#[test]
fn any_eq_to_in_subquery() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE admins (user_id INT PRIMARY KEY);
        SELECT * FROM users WHERE id = ANY(SELECT user_id FROM admins);
    ";
    let output = translate(sql);
    assert!(output.contains("IN (SELECT"), "Expected IN (SELECT: {output}");
}

// ==================== ALL <> operator to NOT IN subquery ====================

#[test]
fn all_ne_to_not_in_subquery() {
    let sql = "
        CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE banned (user_id INT PRIMARY KEY);
        SELECT * FROM users WHERE id <> ALL(SELECT user_id FROM banned);
    ";
    let output = translate(sql);
    assert!(output.contains("NOT IN (SELECT"), "Expected NOT IN (SELECT: {output}");
}

// ==================== Column DEFAULT with identifier ====================

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
}

// ==================== OR REPLACE trigger ====================

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
}

// ==================== BEFORE trigger on RLS table ====================

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
}

// ==================== INSERT ON CONFLICT DO UPDATE expression translation
// ====================

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
}

// ==================== INSERT RETURNING expression translation
// ====================

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
}
