//! Tests for reverse translation (SQLite → PostgreSQL) and roundtrip scenarios.

use pg2sqlite::{options::Pg2SqliteOptions, pg2sqlite::Pg2Sqlite};
use sqlparser::{dialect::SQLiteDialect, parser::Parser};

/// Helper to create a translator with schema from PostgreSQL DDL.
fn setup_translator(pg_ddl: &str) -> Pg2Sqlite {
    Pg2Sqlite::default().sql(pg_ddl).expect("Failed to parse PostgreSQL DDL")
}

/// Helper to parse SQLite SQL.
fn parse_sqlite(sql: &str) -> Vec<sqlparser::ast::Statement> {
    Parser::parse_sql(&SQLiteDialect {}, sql).expect("Failed to parse SQLite SQL")
}

// =============================================================================
// Roundtrip Tests
// =============================================================================

mod roundtrip {
    use super::*;

    #[test]
    fn test_simple_select_roundtrip() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT, email TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        // Parse SQLite SELECT
        let sqlite_sql = "SELECT id, name, email FROM users WHERE name = 'Alice'";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("SELECT"));
        assert!(pg_sql.contains("users"));
        assert!(pg_sql.contains("Alice"));
    }

    #[test]
    fn test_insert_roundtrip() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT, email TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql =
            "INSERT INTO users (id, name, email) VALUES ('abc', 'Alice', 'alice@example.com')";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("INSERT INTO users"));
    }

    #[test]
    fn test_insert_or_ignore_roundtrip() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "INSERT OR IGNORE INTO users (id, name) VALUES ('abc', 'Alice')";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("ON CONFLICT"));
        assert!(pg_sql.contains("DO NOTHING"));
    }

    #[test]
    fn test_insert_or_replace_roundtrip() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT, email TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql =
            "INSERT OR REPLACE INTO users (id, name, email) VALUES ('abc', 'Alice', 'a@b.com')";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("ON CONFLICT"));
        assert!(pg_sql.contains("(id)")); // PK column
        assert!(pg_sql.contains("DO UPDATE SET"));
        assert!(pg_sql.contains("name = EXCLUDED.name"));
        assert!(pg_sql.contains("email = EXCLUDED.email"));
        // PK should NOT be in the SET clause
        assert!(!pg_sql.contains("id = EXCLUDED.id"));
    }

    #[test]
    fn test_insert_or_replace_composite_pk_roundtrip() {
        let pg_ddl = "CREATE TABLE user_roles (user_id UUID, role_id UUID, granted_at TIMESTAMP, PRIMARY KEY (user_id, role_id));";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "INSERT OR REPLACE INTO user_roles (user_id, role_id, granted_at) VALUES ('u1', 'r1', '2024-01-01')";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("ON CONFLICT"));
        assert!(pg_sql.contains("user_id"));
        assert!(pg_sql.contains("role_id"));
        assert!(pg_sql.contains("DO UPDATE SET"));
        assert!(pg_sql.contains("granted_at = EXCLUDED.granted_at"));
    }

    #[test]
    fn test_update_roundtrip() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT, email TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql =
            "UPDATE users SET name = 'Bob', email = 'bob@example.com' WHERE id = 'abc'";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("UPDATE users SET"));
        assert!(pg_sql.contains("name ="));
        assert!(pg_sql.contains("email ="));
        assert!(pg_sql.contains("WHERE"));
    }

    #[test]
    fn test_delete_roundtrip() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "DELETE FROM users WHERE id = 'abc'";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("DELETE FROM users"));
        assert!(pg_sql.contains("WHERE"));
    }

    #[test]
    fn test_datetime_now_roundtrip() {
        let pg_ddl = "CREATE TABLE events (id UUID PRIMARY KEY, created_at TIMESTAMP);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "SELECT * FROM events WHERE created_at > datetime('now')";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("NOW()"));
        assert!(!pg_sql.contains("datetime"));
    }

    #[test]
    fn test_strftime_to_extract_roundtrip() {
        let pg_ddl = "CREATE TABLE events (id UUID PRIMARY KEY, created_at TIMESTAMP);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "SELECT strftime('%Y', created_at) FROM events";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("EXTRACT(YEAR FROM"));
        assert!(!pg_sql.contains("strftime"));
    }

    #[test]
    fn test_instr_to_position_roundtrip() {
        let pg_ddl = "CREATE TABLE docs (id UUID PRIMARY KEY, content TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "SELECT INSTR(content, 'search') FROM docs";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("POSITION"));
        assert!(pg_sql.contains("IN"));
        assert!(!pg_sql.contains("INSTR"));
    }

    #[test]
    fn test_group_concat_to_string_agg_roundtrip() {
        let pg_ddl = "CREATE TABLE tags (id UUID PRIMARY KEY, name TEXT, category TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "SELECT category, group_concat(name) FROM tags GROUP BY category";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("string_agg"));
        assert!(!pg_sql.contains("group_concat"));
    }

    #[test]
    fn test_char_to_chr_roundtrip() {
        let pg_ddl = "CREATE TABLE test (id UUID PRIMARY KEY);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "SELECT char(65) FROM test";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("chr("));
        assert!(!pg_sql.contains("char("));
    }

    /// A SELECT with a JOIN passes through the reverse translator with the
    /// JOIN intact.
    #[test]
    fn test_join_roundtrip() {
        let pg_ddl = "
            CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);
            CREATE TABLE posts (id UUID PRIMARY KEY, user_id UUID, title TEXT);
        ";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "SELECT u.name, p.title FROM users u JOIN posts p ON u.id = p.user_id";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("JOIN"), "Output should contain JOIN: {pg_sql}");
        assert!(pg_sql.contains("users"), "Output should reference 'users': {pg_sql}");
        assert!(pg_sql.contains("posts"), "Output should reference 'posts': {pg_sql}");
    }

    /// A SELECT with GROUP BY and HAVING passes through the reverse translator
    /// with those clauses intact.
    #[test]
    fn test_select_with_group_by_roundtrip() {
        let pg_ddl = "CREATE TABLE items (id UUID PRIMARY KEY, category TEXT, price INTEGER);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql =
            "SELECT category, COUNT(*) AS cnt FROM items GROUP BY category HAVING COUNT(*) > 1";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("GROUP BY"), "Output should contain GROUP BY: {pg_sql}");
        assert!(pg_sql.contains("HAVING"), "Output should contain HAVING: {pg_sql}");
        assert!(pg_sql.contains("category"), "Output should reference 'category': {pg_sql}");
    }

    /// A SELECT with an IN subquery passes through the reverse translator with
    /// the subquery intact.
    #[test]
    fn test_select_with_subquery_roundtrip() {
        let pg_ddl = "
            CREATE TABLE orders (id UUID PRIMARY KEY, user_id UUID, total INTEGER);
            CREATE TABLE users (id UUID PRIMARY KEY, active BOOLEAN);
        ";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql =
            "SELECT * FROM orders WHERE user_id IN (SELECT id FROM users WHERE active = 1)";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("IN"), "Output should contain IN clause: {pg_sql}");
        assert!(pg_sql.contains("SELECT"), "Output should contain nested SELECT: {pg_sql}");
        assert!(pg_sql.contains("users"), "Output should reference 'users': {pg_sql}");
    }
}

// =============================================================================
// Error Cases
// =============================================================================

mod errors {
    use pg2sqlite::errors::Error;

    use super::*;

    #[test]
    fn test_table_not_found_in_schema() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        // Try to INSERT OR REPLACE into a table that doesn't exist in schema
        let sqlite_sql = "INSERT OR REPLACE INTO nonexistent (id, name) VALUES ('abc', 'Alice')";
        let result = translator.reverse_sql(sqlite_sql, &schema, &options);

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            Error::TableNotFoundInSchema { table_name } => {
                assert_eq!(table_name, "nonexistent");
            }
            other => panic!("Expected TableNotFoundInSchema, got: {other:?}"),
        }
    }

    #[test]
    fn test_missing_primary_key_in_upsert() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT, email TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        // INSERT OR REPLACE without the primary key column
        let sqlite_sql = "INSERT OR REPLACE INTO users (name, email) VALUES ('Alice', 'a@b.com')";
        let result = translator.reverse_sql(sqlite_sql, &schema, &options);

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            Error::MissingPrimaryKeyInUpsert { table_name, pk_columns, insert_columns } => {
                assert_eq!(table_name, "users");
                assert!(pk_columns.contains(&"id".to_string()));
                assert!(!insert_columns.contains(&"id".to_string()));
            }
            other => panic!("Expected MissingPrimaryKeyInUpsert, got: {other:?}"),
        }
    }

    #[test]
    fn test_missing_partial_primary_key_in_composite_upsert() {
        let pg_ddl = "CREATE TABLE user_roles (user_id UUID, role_id UUID, granted_at TIMESTAMP, PRIMARY KEY (user_id, role_id));";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        // INSERT OR REPLACE with only one of the composite PK columns
        let sqlite_sql =
            "INSERT OR REPLACE INTO user_roles (user_id, granted_at) VALUES ('u1', '2024-01-01')";
        let result = translator.reverse_sql(sqlite_sql, &schema, &options);

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            Error::MissingPrimaryKeyInUpsert { table_name, pk_columns, insert_columns } => {
                assert_eq!(table_name, "user_roles");
                assert!(pk_columns.contains(&"role_id".to_string()));
                assert!(!insert_columns.contains(&"role_id".to_string()));
            }
            other => panic!("Expected MissingPrimaryKeyInUpsert, got: {other:?}"),
        }
    }

    #[test]
    fn test_or_replace_without_column_list_uses_table_columns() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT, email TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        // No explicit insert column list should still be reversible for OR REPLACE.
        let sqlite_sql = "INSERT OR REPLACE INTO users VALUES ('abc', 'Alice', 'a@b.com')";
        let result = translator.reverse_sql(sqlite_sql, &schema, &options);

        assert!(
            result.is_ok(),
            "Expected OR REPLACE without column list to reverse, got: {result:?}"
        );
        let stmt = &result.unwrap()[0];
        let output = stmt.to_string();
        assert!(output.contains("ON CONFLICT"), "Expected ON CONFLICT in reversed SQL: {output}");
    }

    #[test]
    fn test_insert_or_replace_with_non_public_schema_in_ddl_errors_on_schema_build() {
        let pg_ddl = "CREATE TABLE app.users (id UUID PRIMARY KEY, name TEXT, email TEXT);";
        let translator = setup_translator(pg_ddl);
        let err = translator.build_schema().expect_err(
            "strict schema policy should reject non-public CREATE TABLE in build_schema",
        );
        assert!(
            err.to_string()
                .contains("Only unqualified names and public.<table> names are supported"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_insert_or_replace_with_multiple_non_public_schema_tables_errors_on_schema_build() {
        let pg_ddl = "
            CREATE TABLE app.users (id UUID PRIMARY KEY, name TEXT);
            CREATE TABLE auth.users (id UUID PRIMARY KEY, name TEXT);
        ";
        let translator = setup_translator(pg_ddl);
        let err = translator.build_schema().expect_err(
            "strict schema policy should reject non-public CREATE TABLE in build_schema",
        );
        assert!(
            err.to_string()
                .contains("Only unqualified names and public.<table> names are supported"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_rls_table_detected_in_select() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        // Try to SELECT from an RLS backing table
        let sqlite_sql = "SELECT * FROM users_rls";
        let result = translator.reverse_sql(sqlite_sql, &schema, &options);

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            Error::RlsTableDetected { table_name, suffix } => {
                assert_eq!(table_name, "users_rls");
                assert_eq!(suffix, "_rls");
            }
            other => panic!("Expected RlsTableDetected, got: {other:?}"),
        }
    }

    #[test]
    fn test_rls_table_detected_in_insert() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "INSERT INTO users_rls (id, name) VALUES ('abc', 'Alice')";
        let result = translator.reverse_sql(sqlite_sql, &schema, &options);

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            Error::RlsTableDetected { table_name, .. } => {
                assert_eq!(table_name, "users_rls");
            }
            other => panic!("Expected RlsTableDetected, got: {other:?}"),
        }
    }

    #[test]
    fn test_rls_table_detected_in_update() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "UPDATE users_rls SET name = 'Bob' WHERE id = 'abc'";
        let result = translator.reverse_sql(sqlite_sql, &schema, &options);

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            Error::RlsTableDetected { table_name, .. } => {
                assert_eq!(table_name, "users_rls");
            }
            other => panic!("Expected RlsTableDetected, got: {other:?}"),
        }
    }

    #[test]
    fn test_rls_table_detected_in_delete() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "DELETE FROM users_rls WHERE id = 'abc'";
        let result = translator.reverse_sql(sqlite_sql, &schema, &options);

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            Error::RlsTableDetected { table_name, .. } => {
                assert_eq!(table_name, "users_rls");
            }
            other => panic!("Expected RlsTableDetected, got: {other:?}"),
        }
    }

    #[test]
    fn test_unsupported_reverse_statement_create_table() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        // Try to reverse translate a DDL statement
        let sqlite_stmts = parse_sqlite("CREATE TABLE foo (id INTEGER PRIMARY KEY)");
        let result = translator.reverse_translate(&sqlite_stmts[0], &schema, &options);

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            Error::UnsupportedReverseStatement { .. } => {
                // Statement type format varies, just check we got the right
                // error type
            }
            other => panic!("Expected UnsupportedReverseStatement, got: {other:?}"),
        }
    }

    #[test]
    fn test_unsupported_reverse_statement_drop_table() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_stmts = parse_sqlite("DROP TABLE users");
        let result = translator.reverse_translate(&sqlite_stmts[0], &schema, &options);

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            Error::UnsupportedReverseStatement { .. } => {}
            other => panic!("Expected UnsupportedReverseStatement, got: {other:?}"),
        }
    }
}

// =============================================================================
// Vector Function Roundtrips
// =============================================================================

mod vector_functions {
    use super::*;

    #[test]
    fn test_vec_distance_l2_roundtrip() {
        let pg_ddl = "CREATE TABLE embeddings (id UUID PRIMARY KEY, vec VECTOR(3));";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "SELECT id FROM embeddings ORDER BY vec_distance_L2(vec, '[1,2,3]')";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("<->"));
        assert!(!pg_sql.contains("vec_distance_L2"));
    }

    #[test]
    fn test_vec_distance_cosine_roundtrip() {
        let pg_ddl = "CREATE TABLE embeddings (id UUID PRIMARY KEY, vec VECTOR(3));";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "SELECT id FROM embeddings ORDER BY vec_distance_cosine(vec, '[1,2,3]')";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("<=>"));
        assert!(!pg_sql.contains("vec_distance_cosine"));
    }

    #[test]
    fn test_vec_f32_to_cast_roundtrip() {
        let pg_ddl = "CREATE TABLE embeddings (id UUID PRIMARY KEY, vec VECTOR(3));";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "SELECT vec_f32('[1,2,3]') FROM embeddings";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("::vector"));
        assert!(!pg_sql.contains("vec_f32"));
    }

    #[test]
    fn test_cte_reverse_roundtrip() {
        let pg_ddl = "CREATE TABLE users (id UUID PRIMARY KEY, name TEXT);";
        let translator = setup_translator(pg_ddl);
        let schema = translator.build_schema().unwrap();
        let options = Pg2SqliteOptions::default();

        let sqlite_sql = "WITH cte AS (SELECT * FROM users) SELECT * FROM cte";
        let pg_stmts = translator.reverse_sql(sqlite_sql, &schema, &options).unwrap();

        assert_eq!(pg_stmts.len(), 1);
        let pg_sql = pg_stmts[0].to_string();
        assert!(pg_sql.contains("WITH"), "Expected WITH in CTE roundtrip: {pg_sql}");
        assert!(pg_sql.contains("cte"), "Expected cte alias in CTE roundtrip: {pg_sql}");
        assert!(pg_sql.contains("users"), "Expected users table in CTE roundtrip: {pg_sql}");
    }
}
