//! Implementation of the [`Translator`] trait for the
//! `CreateIndex` type.

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    CreateIndex, Expr, FunctionArguments, Ident, IndexType, ObjectName, ObjectNamePart, Statement,
};

use crate::{
    errors::Error,
    prelude::{Pg2SqliteOptions, Translator},
};

/// Represents the result of translating a GIN/GiST index - either an FTS5
/// virtual table or an error if the index pattern isn't supported.
enum FtsTranslation {
    /// FTS5 virtual table for full-text search
    Fts5 { table_name: ObjectName, columns: Vec<String> },
    /// Index pattern not supported (e.g., JSONB, arrays, spatial data)
    Unsupported(String),
}

/// Extract column identifiers from an expression.
/// This recursively walks the expression tree to find all column references.
fn extract_columns_from_expr(expr: &Expr) -> Vec<String> {
    match expr {
        Expr::Identifier(ident) => vec![ident.value.clone()],
        Expr::CompoundIdentifier(idents) => {
            // For compound identifiers like "table.column", take the last part
            idents.last().map(|i| vec![i.value.clone()]).unwrap_or_default()
        }
        Expr::BinaryOp { left, right, .. } => {
            let mut cols = extract_columns_from_expr(left);
            cols.extend(extract_columns_from_expr(right));
            cols
        }
        Expr::Nested(inner) => extract_columns_from_expr(inner),
        Expr::Function(func) => {
            // Extract columns from function arguments
            if let FunctionArguments::List(list) = &func.args {
                list.args
                    .iter()
                    .flat_map(|arg| {
                        match arg {
                            sqlparser::ast::FunctionArg::Unnamed(
                                sqlparser::ast::FunctionArgExpr::Expr(e),
                            )
                            | sqlparser::ast::FunctionArg::Named {
                                arg: sqlparser::ast::FunctionArgExpr::Expr(e),
                                ..
                            } => extract_columns_from_expr(e),
                            _ => Vec::new(),
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }
        Expr::Cast { expr, .. } => extract_columns_from_expr(expr),
        // For other expression types (values, literals, etc.), return empty
        _ => Vec::new(),
    }
}

/// Check if an expression is a to_tsvector function call and extract its
/// columns.
fn analyze_fts_expression(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::Function(func) => {
            let func_name =
                func.name.0.last().and_then(|p| p.as_ident()).map(|i| i.value.to_lowercase())?;

            if func_name == "to_tsvector" {
                // to_tsvector can have 1 or 2 arguments:
                // to_tsvector(text) or to_tsvector('config', text)
                // We need to extract columns from the text argument(s)
                let columns: Vec<String> = if let FunctionArguments::List(list) = &func.args {
                    list.args
                        .iter()
                        .flat_map(|arg| {
                            match arg {
                                sqlparser::ast::FunctionArg::Unnamed(
                                    sqlparser::ast::FunctionArgExpr::Expr(e),
                                )
                                | sqlparser::ast::FunctionArg::Named {
                                    arg: sqlparser::ast::FunctionArgExpr::Expr(e),
                                    ..
                                } => extract_columns_from_expr(e),
                                _ => Vec::new(),
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                if columns.is_empty() { None } else { Some(columns) }
            } else {
                None
            }
        }
        // Could also be a nested expression containing to_tsvector
        Expr::Nested(inner) => analyze_fts_expression(inner),
        _ => None,
    }
}

/// Analyze a GIN/GiST index to determine how it should be translated.
/// Both GIN and GiST can be used for full-text search with to_tsvector().
fn analyze_fts_index(create_index: &CreateIndex) -> FtsTranslation {
    let table_name = create_index.table_name.clone();
    let index_type = match create_index.using {
        Some(IndexType::GIN) => "GIN",
        Some(IndexType::GiST) => "GiST",
        _ => "Index",
    };

    // Collect all columns from to_tsvector expressions
    let mut fts_columns: Vec<String> = Vec::new();

    for index_col in &create_index.columns {
        if let Some(cols) = analyze_fts_expression(&index_col.column.expr) {
            fts_columns.extend(cols);
        } else {
            // If any column is not a to_tsvector expression, we can't translate
            return FtsTranslation::Unsupported(format!(
                "{index_type} index on expression '{}' is not supported. Only to_tsvector() \
                 expressions can be translated to FTS5. For spatial data, consider SpatiaLite.",
                index_col.column.expr
            ));
        }
    }

    if fts_columns.is_empty() {
        return FtsTranslation::Unsupported(format!(
            "{index_type} index with to_tsvector() must reference at least one column"
        ));
    }

    // Deduplicate columns while preserving order
    let mut seen = std::collections::HashSet::new();
    fts_columns.retain(|col| seen.insert(col.clone()));

    FtsTranslation::Fts5 { table_name, columns: fts_columns }
}

/// Generate an FTS5 virtual table statement.
/// We use regular FTS5 (not external content mode) which stores its own copy
/// of the indexed content. This allows triggers to properly manage
/// insert/update/delete synchronization using standard DELETE statements.
fn create_fts5_virtual_table(base_name: &str, columns: &[String]) -> Statement {
    let fts_name =
        ObjectName(vec![ObjectNamePart::Identifier(Ident::new(format!("{base_name}_fts")))]);

    // Build the module arguments: just column names
    // Using regular FTS5 (no content option) so triggers can use DELETE statements
    let module_args: Vec<Ident> = columns.iter().map(|c| Ident::new(c.clone())).collect();

    Statement::CreateVirtualTable {
        name: fts_name,
        if_not_exists: false,
        module_name: Ident::new("fts5"),
        module_args,
    }
}

/// Generate FTS5 sync triggers.
/// These triggers keep the FTS5 index in sync with the source table using
/// standard DELETE and INSERT statements (not external content 'delete'
/// command).
fn create_fts5_triggers(base_name: &str, pk_column: &str, columns: &[String]) -> Vec<String> {
    let fts_name = format!("{base_name}_fts");
    let columns_list = columns.join(", ");
    let new_values = columns.iter().map(|c| format!("new.{c}")).collect::<Vec<_>>().join(", ");

    vec![
        // AFTER INSERT trigger
        format!(
            "CREATE TRIGGER {base_name}_fts_ai AFTER INSERT ON {base_name} BEGIN \
             INSERT INTO {fts_name}(rowid, {columns_list}) VALUES (new.{pk_column}, {new_values}); \
             END"
        ),
        // AFTER DELETE trigger
        format!(
            "CREATE TRIGGER {base_name}_fts_ad AFTER DELETE ON {base_name} BEGIN \
             DELETE FROM {fts_name} WHERE rowid = old.{pk_column}; \
             END"
        ),
        // AFTER UPDATE trigger
        format!(
            "CREATE TRIGGER {base_name}_fts_au AFTER UPDATE ON {base_name} BEGIN \
             DELETE FROM {fts_name} WHERE rowid = old.{pk_column}; \
             INSERT INTO {fts_name}(rowid, {columns_list}) VALUES (new.{pk_column}, {new_values}); \
             END"
        ),
    ]
}

/// Generate all FTS5 statements (virtual table + sync triggers).
fn create_fts5_statements(
    table_name: &ObjectName,
    columns: &[String],
    schema: &ParserDB,
) -> Result<Vec<Statement>, Error> {
    let base_name = table_name
        .0
        .last()
        .and_then(|p| p.as_ident())
        .map_or_else(|| "unknown".to_string(), |i| i.value.clone());

    // Look up the table to get its primary key column
    let table = schema.table(None, &base_name).ok_or_else(|| {
        Error::UnsupportedSQLiteFeature(format!(
            "Could not find table '{base_name}' in schema for FTS5 index creation"
        ))
    })?;

    let pk_columns: Vec<_> = table.primary_key_columns(schema).collect();
    if pk_columns.len() != 1 {
        return Err(Error::UnsupportedSQLiteFeature(format!(
            "FTS5 requires a single-column primary key. Table '{base_name}' has {} primary key columns.",
            pk_columns.len()
        )));
    }
    let pk_column = pk_columns[0].column_name();

    let mut statements = vec![create_fts5_virtual_table(&base_name, columns)];

    // Add triggers as raw SQL statements
    // We parse them using sqlparser to get proper Statement objects
    for trigger_sql in create_fts5_triggers(&base_name, pk_column, columns) {
        // Since sqlparser may not fully support SQLite trigger syntax,
        // we use a simple approach: wrap the trigger as a raw statement
        // by parsing it. If parsing fails, we skip the trigger.
        if let Ok(parsed) = sqlparser::parser::Parser::parse_sql(
            &sqlparser::dialect::SQLiteDialect {},
            &trigger_sql,
        ) {
            statements.extend(parsed);
        }
    }

    Ok(statements)
}

impl Translator for CreateIndex {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Vec<Statement>;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // Handle GIN/GiST indices - may translate to FTS5 for full-text search
        // Both GIN and GiST can be used with to_tsvector() in PostgreSQL
        if matches!(self.using, Some(IndexType::GIN | IndexType::GiST)) {
            return match analyze_fts_index(self) {
                FtsTranslation::Fts5 { table_name, columns } => {
                    create_fts5_statements(&table_name, &columns, schema)
                }
                FtsTranslation::Unsupported(reason) => Err(Error::UnsupportedSQLiteFeature(reason)),
            };
        }

        // Regular index - translate normally
        Ok(vec![Statement::CreateIndex(CreateIndex {
            columns: self
                .columns
                .iter()
                .map(|col| col.translate(schema, options))
                .collect::<Result<_, _>>()?,
            predicate: self
                .predicate
                .as_ref()
                .map(|predicate| predicate.translate(schema, options))
                .transpose()?,
            ..self.clone()
        })])
    }
}
