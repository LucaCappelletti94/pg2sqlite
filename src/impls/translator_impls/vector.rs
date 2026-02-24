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

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{CreateTable, DataType, Ident, ObjectName, ObjectNamePart, Statement};

use crate::{
    errors::Error,
    impls::{
        generated_sql::parse_generated_sql,
        object_name::{last_ident, schema_and_table_for_lookup},
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
}

/// Check if a data type is a pgvector type (vector or halfvec).
fn is_vector_data_type(data_type: &DataType) -> bool {
    if let DataType::Custom(name, _) = data_type
        && let Some(ident) = last_ident(name)
    {
        let type_name = ident.value.to_ascii_lowercase();
        return type_name == "vector" || type_name == "halfvec";
    }
    false
}

/// Extract dimension count from a vector type like vector(384).
fn extract_dimensions(data_type: &DataType) -> Option<u32> {
    if let DataType::Custom(_, modifiers) = data_type {
        // modifiers contains the type arguments like (384)
        if let Some(first_mod) = modifiers.first() {
            // Try to parse the modifier as a dimension count
            if let Ok(dim) = first_mod.parse::<u32>() {
                return Some(dim);
            }
        }
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
            }
        })
        .collect()
}

/// Find the primary key column(s) for a table.
fn find_pk_column(create_table: &CreateTable, schema: &ParserDB) -> Option<String> {
    // First check table-level constraints
    for constraint in &create_table.constraints {
        if let sqlparser::ast::TableConstraint::PrimaryKey(pk) = constraint
            && let Some(first_col) = pk.columns.first()
            && let sqlparser::ast::Expr::Identifier(ident) = &first_col.column.expr
        {
            return Some(ident.value.clone());
        }
    }

    // Then check column-level PRIMARY KEY constraints
    for col in &create_table.columns {
        for opt in &col.options {
            if matches!(opt.option, sqlparser::ast::ColumnOption::PrimaryKey(_)) {
                return Some(col.name.value.clone());
            }
        }
    }

    // Try to look up from schema
    let (table_schema, table_name) = schema_and_table_for_lookup(&create_table.name);
    if let Some(table_name) = table_name
        && let Some(table) = schema.table(table_schema, table_name)
    {
        let pk_cols: Vec<_> = table.primary_key_columns(schema).collect();
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
    let name = ObjectName(vec![ObjectNamePart::Identifier(Ident::new(vec_table_name.to_string()))]);

    // Build the module arguments for vec0:
    // vec0(pk_id INTEGER PRIMARY KEY, column_name float[N])
    let pk_arg = format!("{pk_column}_id INTEGER PRIMARY KEY");
    let dim_spec = vec_col.dimensions.map_or_else(String::new, |d| format!("[{d}]"));
    let vec_arg = format!("{} float{dim_spec}", vec_col.column_name);

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
    vec![
        // AFTER INSERT trigger
        format!(
            "CREATE TRIGGER {vec_table_name}_ai AFTER INSERT ON {table_name} BEGIN \
             INSERT INTO {vec_table_name} ({pk_column}_id, {column_name}) \
             VALUES (NEW.{pk_column}, NEW.{column_name}); \
             END"
        ),
        // AFTER DELETE trigger
        format!(
            "CREATE TRIGGER {vec_table_name}_ad AFTER DELETE ON {table_name} BEGIN \
             DELETE FROM {vec_table_name} WHERE {pk_column}_id = OLD.{pk_column}; \
             END"
        ),
        // AFTER UPDATE trigger
        format!(
            "CREATE TRIGGER {vec_table_name}_au AFTER UPDATE OF {column_name} ON {table_name} BEGIN \
             UPDATE {vec_table_name} SET {column_name} = NEW.{column_name} \
             WHERE {pk_column}_id = NEW.{pk_column}; \
             END"
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
///
/// # Example output
///
/// For input:
/// ```sql
/// CREATE TABLE items (
///     id INTEGER PRIMARY KEY,
///     embedding vector(384)
/// );
/// ```
///
/// Generates:
/// ```sql
/// CREATE VIRTUAL TABLE items_embedding_vec USING vec0(
///     id_id INTEGER PRIMARY KEY,
///     embedding float[384]
/// );
///
/// CREATE TRIGGER items_embedding_vec_ai AFTER INSERT ON items BEGIN
///     INSERT INTO items_embedding_vec (id_id, embedding)
///     VALUES (NEW.id, NEW.embedding);
/// END;
///
/// CREATE TRIGGER items_embedding_vec_au AFTER UPDATE OF embedding ON items BEGIN
///     UPDATE items_embedding_vec SET embedding = NEW.embedding
///     WHERE id_id = NEW.id;
/// END;
///
/// CREATE TRIGGER items_embedding_vec_ad AFTER DELETE ON items BEGIN
///     DELETE FROM items_embedding_vec WHERE id_id = OLD.id;
/// END;
/// ```
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

    // Look up the table in schema to check for RLS
    let (table_schema, table_name_for_lookup) = schema_and_table_for_lookup(&create_table.name);
    let table_obj = table_name_for_lookup
        .and_then(|table_name| schema.table(table_schema, table_name))
        .ok_or_else(|| {
            Error::UnsupportedSQLiteFeature(format!(
                "Table '{table_name}' not found in schema for vector sync triggers"
            ))
        })?;

    // Determine the correct table for trigger attachment (accounts for RLS)
    let trigger_table_name = resolve_trigger_table_name(&table_name, table_obj, schema, options);

    let dialect = sqlparser::dialect::SQLiteDialect {};
    let mut statements = Vec::new();

    // Generate statements for each vector column
    // If there are multiple vector columns, we create one vec0 table for each
    for vec_col in &vector_cols {
        let vec_table_name = format!("{table_name}_{}_vec", vec_col.column_name);

        // Generate CREATE VIRTUAL TABLE using proper AST
        let create_vec0 = create_vec0_virtual_table(&vec_table_name, &pk_column, vec_col);
        statements.push(create_vec0);

        // Generate triggers (use trigger_table_name for RLS support)
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

#[cfg(test)]
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
}
