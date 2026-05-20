//! Implementation of the [`Translator`] trait for the
//! `CreateIndex` type.

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, TableLike},
};
use sqlparser::{
    ast::{CreateIndex, Expr, Ident, IndexType, ObjectName, ObjectNamePart, Statement},
    dialect::PostgreSqlDialect,
};

use crate::{
    errors::Error,
    impls::{
        generated_sql::parse_generated_sql,
        object_name::{
            last_ident_value_or_display, normalize_schema_qualified_object_name_for_sqlite,
            prefixed_quoted_identifier, quote_identifier, quoted_ident, sql_string_literal,
            sqlite_unqualified_object_name, table_with_implicit_public_lookup,
        },
        shared_helpers::function_argument_exprs,
        translator_impls::{postgis, rls::resolve_trigger_table_name},
    },
    prelude::{Pg2SqliteOptions, TranslationOptions, Translator},
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
            function_argument_exprs(&func.args)
                .into_iter()
                .flat_map(extract_columns_from_expr)
                .collect()
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
                let columns: Vec<String> = function_argument_exprs(&func.args)
                    .into_iter()
                    .flat_map(extract_columns_from_expr)
                    .collect();

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

/// Generate an FTS5 virtual table statement using external content mode.
///
/// External content mode (`content=<table>`) avoids storing a second copy of
/// the indexed text inside the FTS5 table - the content is read from the
/// source table at query time. Sync triggers must use the FTS5 `'delete'`
/// command instead of plain `DELETE` when removing rows from the index.
fn create_fts5_virtual_table(
    base_name: &str,
    content_table: &str,
    pk_column: &str,
    columns: &[String],
) -> Statement {
    let fts_name =
        ObjectName(vec![ObjectNamePart::Identifier(quoted_ident(&format!("{base_name}_fts")))]);

    // Column names followed by external content mode options (unquoted, as
    // SQLite parses FTS5 module args as plain strings).
    let mut module_args: Vec<Ident> = columns.iter().map(|c| quoted_ident(c)).collect();
    module_args.push(Ident::new(format!("content={content_table}")));
    module_args.push(Ident::new(format!("content_rowid={pk_column}")));

    Statement::CreateVirtualTable {
        name: fts_name,
        if_not_exists: false,
        module_name: Ident::new("fts5"),
        module_args,
    }
}

/// Generate FTS5 sync triggers for external content mode.
///
/// These triggers keep the FTS5 index in sync with the source table.
/// The DELETE and UPDATE triggers use the FTS5 `'delete'` command (inserting
/// the special string into the table's own name column) rather than a plain
/// `DELETE`, which is required when the FTS5 table is in external content mode.
///
/// When `predicate_sql` is `Some`, each trigger gets a `WHEN <predicate>`
/// clause so that only rows matching the partial-index predicate are indexed.
fn create_fts5_triggers(
    trigger_table: &str,
    fts_table_base: &str,
    pk_column: &str,
    columns: &[String],
    predicate_sql: Option<&str>,
) -> Vec<String> {
    let fts_name = format!("{fts_table_base}_fts");
    let trigger_table_quoted = quote_identifier(trigger_table);
    let fts_name_quoted = quote_identifier(&fts_name);
    let columns_list = columns.iter().map(|c| quote_identifier(c)).collect::<Vec<_>>().join(", ");
    let new_values =
        columns.iter().map(|c| prefixed_quoted_identifier("new", c)).collect::<Vec<_>>().join(", ");
    let old_values =
        columns.iter().map(|c| prefixed_quoted_identifier("old", c)).collect::<Vec<_>>().join(", ");
    let new_pk = prefixed_quoted_identifier("new", pk_column);
    let old_pk = prefixed_quoted_identifier("old", pk_column);
    let insert_trigger_name = quote_identifier(&format!("{fts_table_base}_fts_ai"));
    let delete_trigger_name = quote_identifier(&format!("{fts_table_base}_fts_ad"));
    let update_trigger_name = quote_identifier(&format!("{fts_table_base}_fts_au"));

    // Build optional WHEN clause; placed between ON <table> and BEGIN.
    let when_clause = predicate_sql.map(|p| format!(" WHEN {p}")).unwrap_or_default();

    vec![
        // AFTER INSERT trigger - attach to the actual table (may be RLS backing table)
        format!(
            "CREATE TRIGGER {insert_trigger_name} AFTER INSERT ON \
             {trigger_table_quoted}{when_clause} BEGIN \
             INSERT INTO {fts_name_quoted}(rowid, {columns_list}) VALUES ({new_pk}, {new_values}); \
             END"
        ),
        // AFTER DELETE trigger - use FTS5 'delete' command (external content mode)
        format!(
            "CREATE TRIGGER {delete_trigger_name} AFTER DELETE ON \
             {trigger_table_quoted}{when_clause} BEGIN \
             INSERT INTO {fts_name_quoted}({fts_name_quoted}, rowid, {columns_list}) \
             VALUES ('delete', {old_pk}, {old_values}); \
             END"
        ),
        // AFTER UPDATE trigger - delete old entry then insert new (external content mode)
        format!(
            "CREATE TRIGGER {update_trigger_name} AFTER UPDATE ON \
             {trigger_table_quoted}{when_clause} BEGIN \
             INSERT INTO {fts_name_quoted}({fts_name_quoted}, rowid, {columns_list}) \
             VALUES ('delete', {old_pk}, {old_values}); \
             INSERT INTO {fts_name_quoted}(rowid, {columns_list}) VALUES ({new_pk}, {new_values}); \
             END"
        ),
    ]
}

/// Generate all FTS5 statements (virtual table + sync triggers + backfill
/// INSERT).
fn create_fts5_statements(
    table_name: &ObjectName,
    columns: &[String],
    predicate_sql: Option<&str>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<Statement>, Error> {
    let base_name = table_name
        .0
        .last()
        .and_then(|p| p.as_ident())
        .map_or_else(|| "unknown".to_string(), |i| i.value.clone());

    // Look up the table to get its primary key column
    let table = table_with_implicit_public_lookup(schema, table_name)?.ok_or_else(|| {
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

    // FTS5 external content mode uses `content_rowid=` to look up rows by SQLite
    // rowid. The rowid must be a 64-bit integer alias, so the PK column must be
    // an integer type. A TEXT or UUID primary key cannot serve as a rowid and
    // would cause FTS5 reads to silently return empty or wrong results.
    let pk_type_str = pk_columns[0].attribute().data_type.to_string().to_uppercase();
    let is_integer_pk = pk_type_str.contains("INT") || pk_type_str.contains("SERIAL");
    if !is_integer_pk {
        return Err(Error::UnsupportedSQLiteFeature(format!(
            "FTS5 external content mode requires an INTEGER primary key column for \
             content_rowid= (table '{base_name}', column '{pk_column}', type '{pk_type_str}'). \
             FTS5 uses the rowid - a 64-bit integer - to look up rows in the content table. \
             Use an INTEGER or BIGINT primary key, or add a surrogate INTEGER rowid column."
        )));
    }

    // Determine the correct table for trigger attachment (accounts for RLS).
    // In external content mode the content table is the same as the trigger
    // table (i.e. the RLS backing table when applicable).
    let trigger_table_name = resolve_trigger_table_name(&base_name, table, schema, options);

    let fts_name = format!("{base_name}_fts");
    let fts_name_quoted = quote_identifier(&fts_name);
    let trigger_table_quoted = quote_identifier(&trigger_table_name);
    let pk_column_quoted = quote_identifier(pk_column);
    let columns_list = columns.iter().map(|c| quote_identifier(c)).collect::<Vec<_>>().join(", ");

    let mut statements =
        vec![create_fts5_virtual_table(&base_name, &trigger_table_name, pk_column, columns)];

    // Add sync triggers as raw SQL statements (parsed to Statement objects).
    for trigger_sql in
        create_fts5_triggers(&trigger_table_name, &base_name, pk_column, columns, predicate_sql)
    {
        let parsed = parse_generated_sql(
            &sqlparser::dialect::SQLiteDialect {},
            &trigger_sql,
            "Failed to parse generated FTS5 trigger SQL",
        )?;
        statements.extend(parsed);
    }

    // Backfill pre-existing rows into the FTS5 index so they are searchable
    // immediately after the index is created.
    let backfill_sql = format!(
        "INSERT INTO {fts_name_quoted}(rowid, {columns_list}) \
         SELECT {pk_column_quoted}, {columns_list} FROM {trigger_table_quoted}"
    );
    let parsed_backfill = parse_generated_sql(
        &sqlparser::dialect::SQLiteDialect {},
        &backfill_sql,
        "Failed to parse generated FTS5 backfill SQL",
    )?;
    statements.extend(parsed_backfill);

    Ok(statements)
}

/// Inspects a GiST `CreateIndex` and, if every indexed column resolves to a
/// `geometry` or `geography` data type in `schema`, returns
/// `SELECT CreateSpatialIndex('tbl','col')` statements (one per column) for
/// geolite to execute at runtime. Returns `Ok(None)` when no indexed column
/// is spatial so the caller can fall through to the FTS5 / error path.
///
/// Errors when:
/// - the GiST has a `WHERE` predicate (geolite's `CreateSpatialIndex` doesn't
///   honor partial indexes).
/// - the GiST mixes spatial and non-spatial columns (silently dropping the
///   non-spatial side would change query semantics).
fn try_spatial_index_routing(
    create_index: &CreateIndex,
    sqlite_table_name: &ObjectName,
    schema: &ParserDB,
) -> Result<Option<Vec<Statement>>, Error> {
    let Some(spatial_columns) = postgis::classify_gist_spatial_columns(create_index, schema)?
    else {
        return Ok(None);
    };

    let table_literal = sql_string_literal(&last_ident_value_or_display(sqlite_table_name));
    let mut statements = Vec::with_capacity(spatial_columns.len());
    for col in spatial_columns {
        let col_literal = sql_string_literal(&col);
        let sql = format!("SELECT CreateSpatialIndex({table_literal}, {col_literal});");
        let mut parsed = parse_generated_sql(
            &PostgreSqlDialect {},
            &sql,
            "spatial index translation (CreateSpatialIndex call)",
        )?;
        statements.append(&mut parsed);
    }
    Ok(Some(statements))
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
        let sqlite_table_name =
            normalize_schema_qualified_object_name_for_sqlite(schema, &self.table_name)?;

        // PostGIS GiST on geometry/geography -> geolite's CreateSpatialIndex.
        // Routed before the FTS5 path so spatial columns don't fall into the
        // tsvector-only analyzer below.
        if options.is_geolite_enabled()
            && matches!(self.using, Some(IndexType::GiST))
            && let Some(spatial_stmts) =
                try_spatial_index_routing(self, &sqlite_table_name, schema)?
        {
            return Ok(spatial_stmts);
        }

        // Handle GIN/GiST indices - may translate to FTS5 for full-text search
        // Both GIN and GiST can be used with to_tsvector() in PostgreSQL
        if matches!(self.using, Some(IndexType::GIN | IndexType::GiST)) {
            // Translate the partial-index predicate (if any) to a SQLite SQL string
            // so that FTS5 sync triggers can include a WHEN clause.
            let predicate_sql: Option<String> = self
                .predicate
                .as_ref()
                .map(|p| p.translate(schema, options).map(|e| e.to_string()))
                .transpose()?;

            let mut normalized_index = self.clone();
            normalized_index.table_name = self.table_name.clone();

            return match analyze_fts_index(&normalized_index) {
                FtsTranslation::Fts5 { table_name, columns } => {
                    create_fts5_statements(
                        &table_name,
                        &columns,
                        predicate_sql.as_deref(),
                        schema,
                        options,
                    )
                }
                FtsTranslation::Unsupported(reason) => Err(Error::UnsupportedSQLiteFeature(reason)),
            };
        }

        // Regular index - translate normally, explicitly dropping PG-only fields
        // (using, concurrently, include, nulls_distinct, with, index_options,
        // alter_options) that are not valid in SQLite.
        Ok(vec![Statement::CreateIndex(CreateIndex {
            name: self.name.clone(),
            table_name: sqlite_unqualified_object_name(&sqlite_table_name),
            using: None,
            columns: self
                .columns
                .iter()
                .map(|col| col.translate(schema, options))
                .collect::<Result<_, _>>()?,
            unique: self.unique,
            concurrently: false,
            if_not_exists: self.if_not_exists,
            include: vec![],
            nulls_distinct: None,
            with: vec![],
            predicate: self
                .predicate
                .as_ref()
                .map(|predicate| predicate.translate(schema, options))
                .transpose()?,
            index_options: vec![],
            alter_options: vec![],
        })])
    }
}

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{
            Expr, FunctionArg, FunctionArgExpr, FunctionArgOperator, FunctionArgumentList,
            FunctionArguments, Ident, Statement,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{analyze_fts_expression, analyze_fts_index, extract_columns_from_expr};
    use crate::prelude::{Pg2SqliteOptions, Translator};

    fn parse_create_index(sql: &str) -> sqlparser::ast::CreateIndex {
        let stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse").remove(0);
        let Statement::CreateIndex(create_index) = stmt else {
            panic!("expected create index");
        };
        create_index
    }

    #[test]
    fn create_index_helpers_cover_named_wildcard_and_non_tsvector_paths() {
        let mut idx =
            parse_create_index("CREATE INDEX idx_docs ON docs USING GIN (to_tsvector(title))");
        let Expr::Function(func) = &mut idx.columns[0].column.expr else {
            panic!("expected function expression");
        };
        func.args = FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                FunctionArg::Named {
                    name: Ident::new("doc"),
                    arg: FunctionArgExpr::Expr(Expr::CompoundIdentifier(vec![
                        Ident::new("docs"),
                        Ident::new("title"),
                    ])),
                    operator: FunctionArgOperator::RightArrow,
                },
                FunctionArg::Unnamed(FunctionArgExpr::Wildcard),
            ],
            clauses: vec![],
        });

        let columns = extract_columns_from_expr(&idx.columns[0].column.expr);
        assert_eq!(columns, vec!["title".to_string()]);

        let nested = Expr::Nested(Box::new(idx.columns[0].column.expr.clone()));
        assert!(analyze_fts_expression(&nested).is_some());

        if let Expr::Function(func) = &mut idx.columns[0].column.expr {
            func.args = FunctionArguments::None;
        }
        assert!(extract_columns_from_expr(&idx.columns[0].column.expr).is_empty());

        let mut idx_non_tsvector =
            parse_create_index("CREATE INDEX idx_docs2 ON docs USING GIN (to_tsvector(title))");
        if let Expr::Function(func) = &mut idx_non_tsvector.columns[0].column.expr {
            func.name =
                sqlparser::ast::ObjectName(vec![sqlparser::ast::ObjectNamePart::Identifier(
                    Ident::new("lower"),
                )]);
        }
        assert!(analyze_fts_expression(&idx_non_tsvector.columns[0].column.expr).is_none());

        let regular = parse_create_index("CREATE INDEX idx_plain ON docs (title)");
        let analysis = analyze_fts_index(&regular);
        assert!(matches!(analysis, super::FtsTranslation::Unsupported(_)));
    }

    #[test]
    fn fts_generation_handles_quoted_table_identifiers() {
        let schema_sql =
            r#"CREATE TABLE "Order Items" ("id" INTEGER PRIMARY KEY, "body text" TEXT);"#;
        let schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, schema_sql).expect("schema SQL should parse"),
            "test".to_string(),
        )
        .expect("schema should build");

        let index = parse_create_index(
            r#"CREATE INDEX order_body_fts ON "Order Items" USING GIN (to_tsvector("body text"))"#,
        );
        let translated =
            index.translate(&schema, &Pg2SqliteOptions::default()).expect("index should translate");

        assert!(
            translated.iter().any(|stmt| stmt.to_string().contains("CREATE TRIGGER")),
            "expected generated FTS synchronization triggers"
        );
    }

    #[test]
    fn analyze_fts_expression_supports_expr_named_arguments() {
        let mut idx =
            parse_create_index("CREATE INDEX idx_docs ON docs USING GIN (to_tsvector(title))");
        let Expr::Function(func) = &mut idx.columns[0].column.expr else {
            panic!("expected function expression");
        };
        func.args = FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                FunctionArg::ExprNamed {
                    name: Expr::Identifier(Ident::new("doc")),
                    arg: FunctionArgExpr::Expr(Expr::CompoundIdentifier(vec![
                        Ident::new("docs"),
                        Ident::new("title"),
                    ])),
                    operator: FunctionArgOperator::Equals,
                },
                FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    sqlparser::ast::ValueWithSpan::from(sqlparser::ast::Value::SingleQuotedString(
                        "english".to_string(),
                    )),
                ))),
            ],
            clauses: vec![],
        });

        let cols =
            analyze_fts_expression(&idx.columns[0].column.expr).expect("should extract cols");
        assert_eq!(cols, vec!["title".to_string()]);
    }
}
