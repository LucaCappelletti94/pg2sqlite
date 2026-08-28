//! Implementation of the [`Translator`] trait for the
//! `CreateIndex` type.

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
    structs::ParserDB,
    traits::{ColumnLike, TableLike},
};
use sqlparser::ast::{
    CreateIndex, Expr, Ident, IndexType, ObjectName, ObjectNamePart, SetExpr, Statement,
    TriggerEvent, TriggerPeriod, VisitMut, VisitorMut,
};

use crate::{
    errors::Error,
    impls::{
        ast_builder,
        function_helpers::{simple_function_expr, string_literal},
        object_name::{
            last_ident_value_or_display, normalize_schema_qualified_object_name_for_sqlite,
            quoted_ident, sqlite_unqualified_object_name, table_with_implicit_public_lookup,
        },
        query_builder::{
            from_relation, make_query, make_simple_select, plain_table_factor, single_expr_query,
        },
        shared_helpers::{
            extract_columns_from_expr, function_argument_exprs,
            nulls_not_distinct_not_supported_error,
        },
        translator_impls::{postgis, rls::resolve_trigger_table_name},
    },
};

/// Represents the result of translating a GIN/GiST index - either an FTS5
/// virtual table or an error if the index pattern isn't supported.
pub(crate) enum FtsTranslation {
    /// FTS5 virtual table for full-text search
    Fts5 { table_name: ObjectName, columns: Vec<String> },
    /// Index pattern not supported (e.g., JSONB, arrays, spatial data)
    Unsupported(String),
}

/// Check if `expr` contains a `to_tsvector` call and return its column list.
fn analyze_fts_expression(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::Function(func) => {
            let func_name =
                func.name.0.last().and_then(|p| p.as_ident()).map(|i| i.value.to_lowercase())?;

            if func_name == "to_tsvector" {
                // to_tsvector can have 1 or 2 arguments:
                // to_tsvector(text) or to_tsvector('config', text)
                let columns: Vec<String> = function_argument_exprs(&func.args)
                    .into_iter()
                    .flat_map(extract_columns_from_expr)
                    .collect();

                if columns.is_empty() { None } else { Some(columns) }
            } else {
                None
            }
        }
        Expr::Nested(inner) => analyze_fts_expression(inner),
        _ => None,
    }
}

/// Analyze a GIN/GiST index and return how to translate it as FTS5.
pub(crate) fn analyze_fts_index(create_index: &CreateIndex) -> FtsTranslation {
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
    let mut seen = alloc::collections::BTreeSet::new();
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

fn qualify_predicate(predicate: &Expr, qualifier: &str) -> Expr {
    use core::ops::ControlFlow;

    struct QualifyIdents(Ident);

    impl VisitorMut for QualifyIdents {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if let Expr::Identifier(column) = expr {
                let column = column.clone();
                *expr = Expr::CompoundIdentifier(vec![self.0.clone(), column]);
            }
            ControlFlow::Continue(())
        }
    }

    let mut qualified_predicate = predicate.clone();
    let _: ControlFlow<()> =
        VisitMut::visit(&mut qualified_predicate, &mut QualifyIdents(Ident::new(qualifier)));
    qualified_predicate
}

/// Generate FTS5 sync triggers for external content mode.
///
/// The DELETE and UPDATE triggers use the FTS5 `'delete'` command (inserting
/// the special string into the table's own name column) rather than a plain
/// `DELETE`, which is required when the FTS5 table is in external content mode.
///
/// When `predicate_sql` is `None`, all three triggers are unconditional.
///
/// When `predicate_sql` is `Some`, the INSERT trigger guards with the
/// NEW-qualified predicate and the DELETE trigger with the OLD-qualified one.
/// The UPDATE trigger cannot use a single WHEN clause for both halves (the
/// delete half needs OLD and the insert half needs NEW), so it is split into
/// two triggers: `{base}_fts_au_delete` (WHEN OLD-qualified) and
/// `{base}_fts_au_insert` (WHEN NEW-qualified). A row crossing the predicate
/// boundary on UPDATE therefore enters or leaves the index correctly.
fn create_fts5_triggers(
    trigger_table: &str,
    fts_table_base: &str,
    pk_column: &str,
    columns: &[String],
    predicate: Option<&Expr>,
) -> Vec<Statement> {
    let fts_name = format!("{fts_table_base}_fts");
    let name = |value: &str| ObjectName(vec![ObjectNamePart::Identifier(quoted_ident(value))]);
    let row_value = |row: &str, column: &str| {
        Expr::CompoundIdentifier(vec![Ident::new(row), quoted_ident(column)])
    };
    let target_columns = || {
        core::iter::once(name("rowid")).chain(columns.iter().map(|column| name(column))).collect()
    };
    let new_values = || {
        core::iter::once(row_value("new", pk_column))
            .chain(columns.iter().map(|column| row_value("new", column)))
            .collect()
    };
    let old_values = || {
        core::iter::once(string_literal("delete"))
            .chain(core::iter::once(row_value("old", pk_column)))
            .chain(columns.iter().map(|column| row_value("old", column)))
            .collect()
    };
    let insert_new = || {
        ast_builder::insert(
            name(&fts_name),
            target_columns(),
            ast_builder::values(vec![new_values()]),
        )
    };
    let delete_old = || {
        let delete_columns = core::iter::once(name(&fts_name)).chain(target_columns()).collect();
        ast_builder::insert(
            name(&fts_name),
            delete_columns,
            ast_builder::values(vec![old_values()]),
        )
    };
    let trigger =
        |suffix: &str, event: TriggerEvent, condition: Option<Expr>, statements: Vec<Statement>| {
            ast_builder::trigger(
                name(&format!("{fts_table_base}_fts_{suffix}")),
                name(trigger_table),
                TriggerPeriod::After,
                event,
                false,
                condition,
                statements,
            )
        };

    match predicate {
        None => {
            vec![
                trigger("ai", TriggerEvent::Insert, None, vec![insert_new()]),
                trigger("ad", TriggerEvent::Delete, None, vec![delete_old()]),
                trigger(
                    "au",
                    TriggerEvent::Update(Vec::new()),
                    None,
                    vec![delete_old(), insert_new()],
                ),
            ]
        }
        Some(predicate) => {
            let new_predicate = qualify_predicate(predicate, "NEW");
            let old_predicate = qualify_predicate(predicate, "OLD");
            vec![
                trigger(
                    "ai",
                    TriggerEvent::Insert,
                    Some(new_predicate.clone()),
                    vec![insert_new()],
                ),
                trigger(
                    "ad",
                    TriggerEvent::Delete,
                    Some(old_predicate.clone()),
                    vec![delete_old()],
                ),
                trigger(
                    "au_delete",
                    TriggerEvent::Update(Vec::new()),
                    Some(old_predicate),
                    vec![delete_old()],
                ),
                trigger(
                    "au_insert",
                    TriggerEvent::Update(Vec::new()),
                    Some(new_predicate),
                    vec![insert_new()],
                ),
            ]
        }
    }
}

/// Generate all FTS5 statements (virtual table + sync triggers + backfill
/// INSERT).
fn create_fts5_statements(
    table_name: &ObjectName,
    columns: &[String],
    predicate: Option<&Expr>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Vec<Statement>, Error> {
    let base_name = table_name
        .0
        .last()
        .and_then(|p| p.as_ident())
        .map_or_else(|| "unknown".to_string(), |i| i.value.clone());

    let table = table_with_implicit_public_lookup(schema, table_name)?.ok_or_else(|| {
        Error::forward_refusal(format!(
            "Could not find table '{base_name}' in schema for FTS5 index creation"
        ))
    })?;

    let pk_columns: Vec<_> = table.primary_key_columns(schema)?.collect();
    if pk_columns.len() != 1 {
        return Err(Error::forward_refusal(format!(
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
        return Err(Error::forward_refusal(format!(
            "FTS5 external content mode requires an INTEGER primary key column for \
         content_rowid= (table '{base_name}', column '{pk_column}', type '{pk_type_str}'). \
         FTS5 uses the rowid - a 64-bit integer - to look up rows in the content table. \
         Use an INTEGER or BIGINT primary key, or add a surrogate INTEGER rowid column."
        )));
    }
    let trigger_table_name = resolve_trigger_table_name(&base_name, table, schema, options)?;

    let name = |value: &str| ObjectName(vec![ObjectNamePart::Identifier(quoted_ident(value))]);
    let mut statements =
        vec![create_fts5_virtual_table(&base_name, &trigger_table_name, pk_column, columns)];
    statements.extend(create_fts5_triggers(
        &trigger_table_name,
        &base_name,
        pk_column,
        columns,
        predicate,
    ));

    let projection = core::iter::once(Expr::Identifier(quoted_ident(pk_column)))
        .chain(columns.iter().map(|column| Expr::Identifier(quoted_ident(column))))
        .collect();
    let source = make_query(
        None,
        SetExpr::Select(Box::new(make_simple_select(
            ast_builder::select_items(projection),
            from_relation(plain_table_factor(name(&trigger_table_name))),
            predicate.cloned(),
        ))),
    );
    let target_columns =
        core::iter::once(name("rowid")).chain(columns.iter().map(|column| name(column))).collect();
    statements.push(ast_builder::insert(name(&format!("{base_name}_fts")), target_columns, source));

    Ok(statements)
}

/// Inspects a GiST `CreateIndex` and, if every indexed column resolves to a
/// `geometry` or `geography` data type in `schema`, returns
/// `SELECT CreateSpatialIndex('tbl','col')` statements (one per column) for
/// SQLiteGIS to execute at runtime. Returns `Ok(None)` when no indexed column
/// is spatial so the caller can fall through to the FTS5 / error path.
///
/// Errors when:
/// - the GiST has a `WHERE` predicate (SQLiteGIS's `CreateSpatialIndex` doesn't
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

    let table_name = last_ident_value_or_display(sqlite_table_name);
    let statements = spatial_columns
        .into_iter()
        .map(|column| {
            Statement::Query(Box::new(single_expr_query(
                simple_function_expr(
                    "CreateSpatialIndex",
                    vec![string_literal(&table_name), string_literal(&column)],
                    None,
                ),
                Vec::new(),
                None,
            )))
        })
        .collect();
    Ok(Some(statements))
}
/// Reports the PostgreSQL-only clauses the regular index path drops.
///
/// Each one is result-neutral, so the index still enforces what it enforced,
/// but D2 puts a result-neutral drop in the warn bucket rather than the silent
/// one. Only the clauses a `PostgreSqlDialect` parse can deliver are here:
/// `index_options` and `alter_options` belong to other dialects and never
/// arrive populated.
fn report_dropped_index_clauses(index: &CreateIndex, emit: crate::warnings::WarningSink<'_>) {
    if !index.include.is_empty() {
        let included = index.include.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
        let key = index.columns.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
        emit(crate::warnings::TranslationWarning::LossyDowngrade {
            construct: "index INCLUDE".to_string(),
            from: format!("({key}) INCLUDE ({included})"),
            to: format!("({key})"),
            location: index.table_name.to_string(),
            reason: "SQLite has no covering index, so a query PostgreSQL answers from the index \
                     alone, an index-only scan, visits the table here instead. An INCLUDE \
                     column is payload rather than key, so nothing about which rows the index \
                     accepts changes."
                .to_string(),
        });
    }

    if let Some(method) = &index.using {
        emit(crate::warnings::TranslationWarning::LossyDowngrade {
            construct: "index method".to_string(),
            from: format!("USING {method}"),
            to: "a SQLite b-tree index".to_string(),
            location: index.table_name.to_string(),
            reason: "SQLite builds one kind of index, so the emitted one serves whatever a \
                     b-tree serves. For hash and BRIN that loses nothing, since a b-tree \
                     answers equality and ranges alike, but a method chosen for a query shape \
                     a b-tree cannot serve no longer serves it."
                .to_string(),
        });
    }

    if index.concurrently {
        emit(crate::warnings::TranslationWarning::LossyDrop {
            construct: "CREATE INDEX CONCURRENTLY".to_string(),
            reason: "SQLite builds an index while holding a write lock and has no concurrent \
                     form. The resulting index is identical, so only the lock the build takes \
                     differs."
                .to_string(),
        });
    }

    if !index.with.is_empty() {
        let parameters = index.with.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
        emit(crate::warnings::TranslationWarning::LossyDrop {
            construct: "index storage parameters".to_string(),
            reason: format!(
                "SQLite has no storage parameters, so WITH ({parameters}) is dropped. These \
                 tune how PostgreSQL lays the index out on disk and say nothing about what it \
                 answers."
            ),
        });
    }
}

crate::traits::translator::impl_contextual_translator!(CreateIndex => Vec<Statement>);
impl crate::traits::translator::TranslatorWithContext for CreateIndex {
    fn translate_with_warnings(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        let sqlite_table_name =
            normalize_schema_qualified_object_name_for_sqlite(schema, &self.table_name)?;

        // PostGIS GiST on geometry/geography -> SQLiteGIS's CreateSpatialIndex.
        // Routed before the FTS5 path so spatial columns don't fall into the
        // tsvector-only analyzer below.
        if options.is_sqlitegis_enabled()
            && matches!(self.using, Some(IndexType::GiST))
            && let Some(spatial_stmts) =
                try_spatial_index_routing(self, &sqlite_table_name, schema)?
        {
            return Ok(spatial_stmts);
        }

        if matches!(self.using, Some(IndexType::GIN | IndexType::GiST)) {
            let predicate = self
                .predicate
                .as_ref()
                .map(|predicate| predicate.translate_with_warnings(schema, options, emit))
                .transpose()?;

            let mut normalized_index = self.clone();
            normalized_index.table_name = self.table_name.clone();

            return match analyze_fts_index(&normalized_index) {
                FtsTranslation::Fts5 { table_name, columns } => {
                    create_fts5_statements(
                        &table_name,
                        &columns,
                        predicate.as_ref(),
                        schema,
                        options,
                    )
                }
                FtsTranslation::Unsupported(reason) => Err(Error::forward_refusal(reason)),
            };
        }

        if self.nulls_distinct == Some(false) {
            return Err(nulls_not_distinct_not_supported_error());
        }

        report_dropped_index_clauses(self, emit);

        // Regular index - translate normally, explicitly dropping PG-only fields
        // (using, concurrently, include, nulls_distinct, with, index_options,
        // alter_options) that are not valid in SQLite. `nulls_distinct` is only
        // safe to drop for the DISTINCT spelling, refused above otherwise,
        // because it decides which rows collide.
        Ok(vec![Statement::CreateIndex(CreateIndex {
            name: self.name.clone(),
            table_name: sqlite_unqualified_object_name(&sqlite_table_name),
            using: None,
            columns: self
                .columns
                .iter()
                .map(|col| col.translate_with_warnings(schema, options, emit))
                .collect::<Result<_, _>>()?,
            unique: self.unique,
            concurrently: false,
            r#async: false,
            if_not_exists: self.if_not_exists,
            include: vec![],
            nulls_distinct: None,
            with: vec![],
            predicate: self
                .predicate
                .as_ref()
                .map(|predicate| predicate.translate_with_warnings(schema, options, emit))
                .transpose()?,
            index_options: vec![],
            alter_options: vec![],
        })])
    }
}

#[cfg(all(test, feature = "std"))]
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
        let named = extract_columns_from_expr(&idx.columns[0].column.expr);
        assert!(named.is_empty(), "the expression names no column, got {named:?}");

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
