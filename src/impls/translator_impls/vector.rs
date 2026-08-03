//! Vector translation from pgvector to sqlite-vec.
//!
//! This module handles the translation of pgvector columns to sqlite-vec by
//! generating:
//! 1. The main table with vector columns as BLOB
//! 2. A companion vec0 virtual table for KNN search
//! 3. INSERT/UPDATE/DELETE triggers to keep the vec0 table synchronized
//!
//! # Performance Limitation
//!
//! **Important:** As of sqlite-vec v0.1.x, vec0 uses **brute-force search
//! only** (no ANN indexing like HNSW or IVFFlat). This means queries are O(n)
//! rather than O(log n). For large datasets (>100k vectors), this may be slow
//! compared to pgvector's indexed search.
//!
//! The sqlite-vec project is actively working on ANN support:
//! <https://github.com/asg017/sqlite-vec/issues/25>
//!
//! The translation is correct and will automatically benefit when ANN is added.
//! In the meantime, consider:
//! - Binary quantization (`vec_quantize_binary()`) for ~25x constant factor
//!   speedup
//! - Pre-filtering with WHERE clauses to reduce scan size
//! - Keeping datasets under 100k vectors for acceptable latency

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use sql_traits::{
    errors::LookupError,
    structs::ParserDB,
    traits::{ColumnLike, TableLike},
};
use sqlparser::ast::{
    CreateTable, DataType, Expr, Ident, ObjectName, ObjectNamePart, Statement, Value, ValueWithSpan,
};

use crate::{
    errors::Error,
    impls::{
        function_helpers::simple_function_expr,
        generated_sql::parse_generated_sql,
        object_name::{
            last_ident, prefixed_quoted_identifier, quote_identifier, quoted_ident,
            table_with_implicit_public_lookup,
        },
        translator_impls::rls::resolve_trigger_table_name,
    },
    prelude::Pg2SqliteOptions,
};

/// Information about a vector column in a table.
#[derive(Debug, Clone)]
pub struct VectorColumnInfo {
    /// The name of the column.
    pub column_name: String,
    /// The number of dimensions (e.g., 384 for vector(384)).
    pub dimensions: Option<u32>,
    /// Whether this is a half-precision (16-bit float) vector column.
    /// `true` for `halfvec`, `false` for `vector`.
    pub is_halfvec: bool,
}

/// Check if a data type is a pgvector type (vector or halfvec).
pub(crate) fn is_vector_data_type(data_type: &DataType) -> bool {
    if let DataType::Custom(name, _) = data_type
        && let Some(ident) = last_ident(name)
    {
        let type_name = ident.value.to_ascii_lowercase();
        return type_name == "vector" || type_name == "halfvec";
    }
    false
}

/// Check if a data type is the halfvec (16-bit float) type.
pub(crate) fn is_halfvec_data_type(data_type: &DataType) -> bool {
    if let DataType::Custom(name, _) = data_type
        && let Some(ident) = last_ident(name)
    {
        return ident.value.eq_ignore_ascii_case("halfvec");
    }
    false
}

/// Extract dimension count from a vector type like vector(384).
fn extract_dimensions(data_type: &DataType) -> Option<u32> {
    // The modifier list carries the type arguments, so `vector(384)` arrives
    // as the single modifier "384".
    if let DataType::Custom(_, modifiers) = data_type
        && let Some(first_mod) = modifiers.first()
        && let Ok(dim) = first_mod.parse::<u32>()
    {
        return Some(dim);
    }
    None
}

/// Check if a CREATE TABLE has any vector columns.
#[must_use]
pub fn has_vector_columns(create_table: &CreateTable) -> bool {
    create_table.columns.iter().any(|col| is_vector_data_type(&col.data_type))
}

/// Extract all vector column information from a CREATE TABLE.
#[must_use]
pub fn extract_vector_columns(create_table: &CreateTable) -> Vec<VectorColumnInfo> {
    create_table
        .columns
        .iter()
        .filter(|col| is_vector_data_type(&col.data_type))
        .map(|col| {
            VectorColumnInfo {
                column_name: col.name.value.clone(),
                dimensions: extract_dimensions(&col.data_type),
                is_halfvec: is_halfvec_data_type(&col.data_type),
            }
        })
        .collect()
}

/// Find the primary key column(s) for a table.
fn find_pk_column(create_table: &CreateTable, schema: &ParserDB) -> Option<String> {
    for constraint in &create_table.constraints {
        if let sqlparser::ast::TableConstraint::PrimaryKey(pk) = constraint
            && let Some(first_col) = pk.columns.first()
            && let sqlparser::ast::Expr::Identifier(ident) = &first_col.column.expr
        {
            return Some(ident.value.clone());
        }
    }

    for col in &create_table.columns {
        for opt in &col.options {
            if matches!(opt.option, sqlparser::ast::ColumnOption::PrimaryKey(_)) {
                return Some(col.name.value.clone());
            }
        }
    }

    if let Ok(Some(table)) = table_with_implicit_public_lookup(schema, &create_table.name)
        && let Ok(pk_iter) = table.primary_key_columns(schema)
    {
        let pk_cols: Vec<_> = pk_iter.collect();
        if pk_cols.len() == 1 {
            return Some(pk_cols[0].column_name().to_string());
        }
    }

    None
}

/// Create a vec0 virtual table statement.
fn create_vec0_virtual_table(
    vec_table_name: &str,
    pk_column: &str,
    vec_col: &VectorColumnInfo,
) -> Statement {
    let name = ObjectName(vec![ObjectNamePart::Identifier(quoted_ident(vec_table_name))]);

    // Build the module arguments for vec0:
    // vec0(pk_id INTEGER PRIMARY KEY, column_name float[N])
    let pk_arg = format!("{} INTEGER PRIMARY KEY", quote_identifier(&format!("{pk_column}_id")));
    let dim_spec = vec_col.dimensions.map_or_else(String::new, |d| format!("[{d}]"));
    // halfvec uses 16-bit floats (float16); vector uses 32-bit floats (float)
    let vec_type = if vec_col.is_halfvec { "float16" } else { "float" };
    let vec_arg = format!("{} {vec_type}{dim_spec}", quote_identifier(&vec_col.column_name));

    let module_args = vec![Ident::new(pk_arg), Ident::new(vec_arg)];

    Statement::CreateVirtualTable {
        name,
        if_not_exists: false,
        module_name: Ident::new("vec0"),
        module_args,
    }
}

/// Create vec0 sync triggers.
fn create_vec0_triggers(
    table_name: &str,
    vec_table_name: &str,
    pk_column: &str,
    column_name: &str,
) -> Vec<String> {
    let trigger_table_quoted = quote_identifier(table_name);
    let vec_table_quoted = quote_identifier(vec_table_name);
    let vec_pk_column = format!("{pk_column}_id");
    let vec_pk_column_quoted = quote_identifier(&vec_pk_column);
    let column_name_quoted = quote_identifier(column_name);
    let new_pk = prefixed_quoted_identifier("NEW", pk_column);
    let old_pk = prefixed_quoted_identifier("OLD", pk_column);
    let new_vec_col = prefixed_quoted_identifier("NEW", column_name);
    let insert_trigger_name = quote_identifier(&format!("{vec_table_name}_ai"));
    let delete_trigger_name = quote_identifier(&format!("{vec_table_name}_ad"));
    let update_trigger_name = quote_identifier(&format!("{vec_table_name}_au"));

    vec![
        format!(
            "CREATE TRIGGER {insert_trigger_name} AFTER INSERT ON {trigger_table_quoted} BEGIN \
             INSERT INTO {vec_table_quoted} ({vec_pk_column_quoted}, {column_name_quoted}) \
             VALUES ({new_pk}, {new_vec_col}); \
             END"
        ),
        format!(
            "CREATE TRIGGER {delete_trigger_name} AFTER DELETE ON {trigger_table_quoted} BEGIN \
             DELETE FROM {vec_table_quoted} WHERE {vec_pk_column_quoted} = {old_pk}; \
             END"
        ),
        // Update vector sync on vector value or PK changes.
        format!(
            "CREATE TRIGGER {update_trigger_name} AFTER UPDATE OF {column_name_quoted}, {} ON {trigger_table_quoted} BEGIN \
             UPDATE {vec_table_quoted} SET {column_name_quoted} = {new_vec_col}, {vec_pk_column_quoted} = {new_pk} \
             WHERE {vec_pk_column_quoted} = {old_pk}; \
             END",
            quote_identifier(pk_column)
        ),
    ]
}

/// Generate vec0 virtual table and trigger statements for a table with vector
/// columns.
///
/// # Errors
///
/// Returns an error if the table has vector columns but no single-column
/// primary key, as this is required for the synchronization triggers.
pub fn generate_vec0_statements(
    create_table: &CreateTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, Error> {
    let vector_cols = extract_vector_columns(create_table);
    if vector_cols.is_empty() {
        return Ok(Vec::new());
    }

    let table_name = last_ident(&create_table.name)
        .map_or_else(|| create_table.name.to_string(), |ident| ident.value.clone());
    let pk_column = find_pk_column(create_table, schema).ok_or_else(|| {
        Error::UnsupportedSQLiteFeature(format!(
            "Table '{table_name}' with vector columns requires a single-column primary key \
             for sqlite-vec synchronization triggers."
        ))
    })?;

    let table_obj =
        table_with_implicit_public_lookup(schema, &create_table.name)?.ok_or_else(|| {
            Error::UnsupportedSQLiteFeature(format!(
                "Table '{table_name}' not found in schema for vector sync triggers"
            ))
        })?;

    let trigger_table_name = resolve_trigger_table_name(&table_name, table_obj, schema, options)?;

    let dialect = sqlparser::dialect::SQLiteDialect {};
    let mut statements = Vec::new();

    for vec_col in &vector_cols {
        let vec_table_name = format!("{table_name}_{}_vec", vec_col.column_name);

        let create_vec0 = create_vec0_virtual_table(&vec_table_name, &pk_column, vec_col);
        statements.push(create_vec0);

        for trigger_sql in create_vec0_triggers(
            &trigger_table_name,
            &vec_table_name,
            &pk_column,
            &vec_col.column_name,
        ) {
            let parsed = parse_generated_sql(
                &dialect,
                &trigger_sql,
                "Failed to parse generated vec0 synchronization trigger SQL",
            )?;
            statements.extend(parsed);
        }
    }

    Ok(statements)
}

/// Build a `vec_f32(expr)` or `vec_f16(expr)` function call.
fn make_vec_conversion_call(arg: Expr, is_halfvec: bool) -> Expr {
    let func_name = if is_halfvec { "vec_f16" } else { "vec_f32" };
    simple_function_expr(func_name, vec![arg], None)
}

/// If `expr` is a single-quoted string literal, wrap it with the matching
/// sqlite-vec conversion function so SQLite STRICT tables accept it in the
/// BLOB column. Other expression shapes pass through unchanged. NULL,
/// DEFAULT, identifiers, casts, and existing function calls (including
/// the `vec_f32` / `vec_f16` calls the cast translator already lowers
/// `'[...]'::vector` to) are left alone, so this helper is idempotent.
pub(crate) fn maybe_wrap_text_vector_literal(expr: Expr, is_halfvec: bool) -> Expr {
    if matches!(&expr, Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(_), .. })) {
        make_vec_conversion_call(expr, is_halfvec)
    } else {
        expr
    }
}

/// Collect `(column_name, is_halfvec)` pairs for every pgvector column on
/// the resolved schema table, preserving column ordinal order. Returns an
/// empty `Vec` when the table has no vector columns.
pub(crate) fn vector_columns_of_table(
    table: &<ParserDB as sql_traits::traits::DatabaseLike>::Table,
    schema: &ParserDB,
) -> Result<Vec<(String, bool)>, LookupError> {
    Ok(table
        .columns(schema)?
        .filter_map(|col| {
            let dt = &col.attribute().data_type;
            if is_vector_data_type(dt) {
                Some((col.column_name().to_string(), is_halfvec_data_type(dt)))
            } else {
                None
            }
        })
        .collect())
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{ColumnOption, Statement, TableConstraint},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{find_pk_column, generate_vec0_statements};
    use crate::prelude::Pg2SqliteOptions;

    fn parse_statements(sql: &str) -> Vec<Statement> {
        Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse")
    }

    fn parse_create_table(sql: &str) -> sqlparser::ast::CreateTable {
        let stmt = parse_statements(sql).remove(0);
        let Statement::CreateTable(create_table) = stmt else {
            panic!("expected create table");
        };
        create_table
    }

    #[test]
    fn find_pk_column_falls_back_to_schema_metadata() {
        let schema_sql = "CREATE TABLE docs(id INTEGER PRIMARY KEY, embedding vector(3));";
        let schema = ParserDB::from_statements(parse_statements(schema_sql), "test".to_string())
            .expect("schema should build");

        let mut create_table = parse_create_table(schema_sql);
        for col in &mut create_table.columns {
            col.options.retain(|opt| !matches!(opt.option, ColumnOption::PrimaryKey(_)));
        }
        create_table.constraints.retain(|c| !matches!(c, TableConstraint::PrimaryKey(_)));

        assert_eq!(find_pk_column(&create_table, &schema).as_deref(), Some("id"));
    }

    #[test]
    fn generate_vec0_statements_returns_empty_for_tables_without_vector_columns() {
        let sql = "CREATE TABLE plain(id INTEGER PRIMARY KEY, name TEXT);";
        let schema = ParserDB::from_statements(parse_statements(sql), "test".to_string())
            .expect("schema should build");
        let create_table = parse_create_table(sql);
        let options = Pg2SqliteOptions::default();

        let statements =
            generate_vec0_statements(&create_table, &schema, &options).expect("should succeed");
        assert!(statements.is_empty());
    }

    #[test]
    fn generate_vec0_statements_errors_when_table_is_missing_from_schema() {
        let create_table = parse_create_table(
            "CREATE TABLE missing(id INTEGER PRIMARY KEY, embedding vector(3));",
        );
        let schema =
            ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build");
        let options = Pg2SqliteOptions::default();

        let err = generate_vec0_statements(&create_table, &schema, &options)
            .expect_err("missing table should error");
        assert!(err.to_string().contains("not found in schema"));
    }

    #[test]
    fn generate_vec0_statements_supports_quoted_identifiers() {
        let sql = r#"CREATE TABLE "Vector Docs" ("doc id" INTEGER PRIMARY KEY, "embedding vec" vector(3));"#;
        let schema = ParserDB::from_statements(parse_statements(sql), "test".to_string())
            .expect("schema should build");
        let create_table = parse_create_table(sql);
        let options = Pg2SqliteOptions::default();

        let statements = generate_vec0_statements(&create_table, &schema, &options)
            .expect("quoted identifiers should translate");
        assert!(
            statements.iter().any(|stmt| stmt.to_string().contains("CREATE TRIGGER")),
            "expected vec synchronization triggers"
        );
    }

    #[test]
    fn generate_vec0_update_trigger_tracks_primary_key_changes() {
        let sql = "CREATE TABLE docs(id INTEGER PRIMARY KEY, embedding vector(3));";
        let schema = ParserDB::from_statements(parse_statements(sql), "test".to_string())
            .expect("schema should build");
        let create_table = parse_create_table(sql);
        let options = Pg2SqliteOptions::default();

        let statements =
            generate_vec0_statements(&create_table, &schema, &options).expect("should succeed");
        let update_trigger_sql = statements
            .iter()
            .map(ToString::to_string)
            .find(|sql| sql.contains("_au"))
            .expect("update trigger should be generated");

        assert!(
            update_trigger_sql.contains("AFTER UPDATE OF embedding, id"),
            "update trigger should fire on vector or PK updates: {update_trigger_sql}"
        );
        assert!(
            update_trigger_sql.contains("id_id = NEW.id"),
            "update trigger should update vector row identity: {update_trigger_sql}"
        );
        assert!(
            update_trigger_sql.contains("WHERE id_id = OLD.id"),
            "update trigger should match previous PK value: {update_trigger_sql}"
        );
    }
}
