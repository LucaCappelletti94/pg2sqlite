//! Implementation of the [`Translator`] trait for the
//! `Expr` type.

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    BinaryOperator, Expr, Function, FunctionArguments, Ident, ObjectName, ObjectNamePart, Query,
    Select, SelectFlavor, SelectItem, SetExpr, TableFactor, TableWithJoins, Value, ValueWithSpan,
    helpers::attached_token::AttachedToken,
};

use crate::prelude::{Pg2SqliteOptions, Translator};

/// Extract the table name from a to_tsvector expression by analyzing the
/// columns. Returns the table name if we can infer it from the column
/// references.
fn extract_table_from_tsvector(func: &Function, schema: &ParserDB) -> Option<String> {
    // Get columns from to_tsvector arguments
    let columns = extract_columns_from_function(func);

    if columns.is_empty() {
        return None;
    }

    // Try to find which table contains these columns
    // For simplicity, we look for a table that has a GIN/GiST index on these
    // columns by checking which tables exist in the schema
    for table in schema.tables() {
        let table_name = table.table_name();
        let table_columns: std::collections::HashSet<_> =
            table.columns(schema).map(|c| c.column_name().to_lowercase()).collect();

        // Check if all referenced columns belong to this table
        if columns.iter().all(|col| table_columns.contains(&col.to_lowercase())) {
            return Some(table_name.to_string());
        }
    }

    None
}

/// Extract column names from a function's arguments (recursively).
fn extract_columns_from_function(func: &Function) -> Vec<String> {
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

/// Extract column identifiers from an expression.
fn extract_columns_from_expr(expr: &Expr) -> Vec<String> {
    match expr {
        Expr::Identifier(ident) => vec![ident.value.clone()],
        Expr::CompoundIdentifier(idents) => {
            idents.last().map(|i| vec![i.value.clone()]).unwrap_or_default()
        }
        Expr::BinaryOp { left, right, .. } => {
            let mut cols = extract_columns_from_expr(left);
            cols.extend(extract_columns_from_expr(right));
            cols
        }
        Expr::Nested(inner) => extract_columns_from_expr(inner),
        Expr::Function(func) => extract_columns_from_function(func),
        Expr::Cast { expr, .. } => extract_columns_from_expr(expr),
        _ => Vec::new(),
    }
}

/// Extract the search query string from a to_tsquery expression.
fn extract_query_from_tsquery(func: &Function) -> Option<String> {
    if let FunctionArguments::List(list) = &func.args {
        // to_tsquery can have 1 or 2 args: to_tsquery('query') or to_tsquery('config',
        // 'query') The query is always the last argument
        for arg in list.args.iter().rev() {
            if let sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(
                Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(s), .. }),
            ))
            | sqlparser::ast::FunctionArg::Named {
                arg:
                    sqlparser::ast::FunctionArgExpr::Expr(Expr::Value(ValueWithSpan {
                        value: Value::SingleQuotedString(s),
                        ..
                    })),
                ..
            } = arg
            {
                return Some(s.clone());
            }
        }
    }
    None
}

/// Translate PostgreSQL tsquery syntax to FTS5 MATCH syntax.
/// - `&` (AND) -> space (implicit AND in FTS5)
/// - `|` (OR) -> `OR`
/// - `!` (NOT) -> `NOT`
/// - `<->` and `<N>` (phrase/proximity) -> not directly supported, use space
/// - `:*` (prefix) -> `*` (FTS5 prefix syntax)
fn translate_tsquery_to_fts5(tsquery: &str) -> String {
    tsquery
        .replace(":*", "*") // PostgreSQL prefix syntax to FTS5 prefix syntax
        .replace('&', " ")
        .replace('|', " OR ")
        .replace('!', " NOT ")
        .replace("<->", " ")
        // Remove any remaining angle bracket operators like <2>
        .chars()
        .filter(|c| *c != '<' && *c != '>')
        .collect::<String>()
        // Clean up multiple spaces
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Check if a function is to_tsvector.
fn is_to_tsvector(func: &Function) -> bool {
    func.name
        .0
        .last()
        .and_then(|p| p.as_ident())
        .is_some_and(|i| i.value.to_lowercase() == "to_tsvector")
}

/// Check if a function is to_tsquery or plainto_tsquery or phraseto_tsquery.
fn is_to_tsquery(func: &Function) -> bool {
    func.name.0.last().and_then(|p| p.as_ident()).is_some_and(|i| {
        let name = i.value.to_lowercase();
        name == "to_tsquery" || name == "plainto_tsquery" || name == "phraseto_tsquery"
    })
}

/// Translate a full-text search expression (to_tsvector @@ to_tsquery) to FTS5
/// MATCH. Returns an expression like: pk_col IN (SELECT rowid FROM table_fts
/// WHERE table_fts MATCH 'query')
fn translate_fts_expression(
    tsvector_func: &Function,
    tsquery_func: &Function,
    schema: &ParserDB,
) -> Result<Expr, crate::errors::Error> {
    // Get the table name from the tsvector expression
    let table_name = extract_table_from_tsvector(tsvector_func, schema).ok_or_else(|| {
        crate::errors::Error::UnsupportedSQLiteFeature(
            "Could not determine table name from to_tsvector expression. \
             Ensure the columns referenced exist in a table with a GIN/GiST index."
                .to_string(),
        )
    })?;

    // Look up the table to get its primary key column
    let table = schema.table(None, &table_name).ok_or_else(|| {
        crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "Could not find table '{table_name}' in schema for FTS5 query translation"
        ))
    })?;

    let pk_columns: Vec<_> = table.primary_key_columns(schema).collect();
    if pk_columns.len() != 1 {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "FTS5 requires a single-column primary key. Table '{table_name}' has {} primary key columns.",
            pk_columns.len()
        )));
    }
    let pk_column = pk_columns[0].column_name();

    // Get the search query from tsquery
    let query_str = extract_query_from_tsquery(tsquery_func).ok_or_else(|| {
        crate::errors::Error::UnsupportedSQLiteFeature(
            "Could not extract search query from to_tsquery expression. \
             Only string literal arguments are supported (e.g., to_tsquery('search term')). \
             Parameterized queries like to_tsquery($1) are not yet supported."
                .to_string(),
        )
    })?;

    // Translate tsquery syntax to FTS5 syntax
    let fts5_query = translate_tsquery_to_fts5(&query_str);
    let fts_table_name = format!("{table_name}_fts");

    // Build: pk_col IN (SELECT rowid FROM table_fts WHERE table_fts MATCH 'query')
    Ok(Expr::InSubquery {
        expr: Box::new(Expr::Identifier(Ident::new(pk_column))),
        subquery: Box::new(Query {
            with: None,
            body: Box::new(SetExpr::Select(Box::new(Select {
                select_token: AttachedToken::empty(),
                distinct: None,
                top: None,
                top_before_distinct: false,
                projection: vec![SelectItem::UnnamedExpr(Expr::Identifier(Ident::new("rowid")))],
                into: None,
                from: vec![TableWithJoins {
                    relation: TableFactor::Table {
                        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(
                            fts_table_name.clone(),
                        ))]),
                        alias: None,
                        args: None,
                        with_hints: Vec::new(),
                        version: None,
                        with_ordinality: false,
                        partitions: Vec::new(),
                        json_path: None,
                        sample: None,
                        index_hints: Vec::new(),
                    },
                    joins: Vec::new(),
                }],
                lateral_views: Vec::new(),
                prewhere: None,
                selection: Some(Expr::BinaryOp {
                    left: Box::new(Expr::Identifier(Ident::new(fts_table_name.clone()))),
                    op: BinaryOperator::Match,
                    right: Box::new(Expr::Value(ValueWithSpan {
                        value: Value::SingleQuotedString(fts5_query),
                        span: sqlparser::tokenizer::Span::empty(),
                    })),
                }),
                group_by: sqlparser::ast::GroupByExpr::Expressions(Vec::new(), Vec::new()),
                cluster_by: Vec::new(),
                distribute_by: Vec::new(),
                sort_by: Vec::new(),
                having: None,
                named_window: Vec::new(),
                qualify: None,
                window_before_qualify: false,
                value_table_mode: None,
                connect_by: None,
                flavor: SelectFlavor::Standard,
                exclude: None,
                optimizer_hint: None,
            }))),
            order_by: None,
            limit_clause: None,
            fetch: None,
            locks: Vec::new(),
            for_clause: None,
            settings: None,
            format_clause: None,
            pipe_operators: Vec::new(),
        }),
        negated: false,
    })
}

impl Translator for Expr {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Self;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        Ok(match self {
            Expr::Function(func) => Expr::Function(func.translate(schema, options)?),
            // Pass through simple expressions that work in SQLite
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Value(_) => self.clone(),
            // Handle unary operators (e.g., -1, NOT x)
            Expr::UnaryOp { op, expr } => {
                Expr::UnaryOp { op: *op, expr: Box::new(expr.translate(schema, options)?) }
            }
            // Handle nested/parenthesized expressions
            Expr::Nested(inner) => Expr::Nested(Box::new(inner.translate(schema, options)?)),
            // Handle binary operations (e.g., 1 + 2, a || b)
            Expr::BinaryOp { left, op, right } => {
                // Check for full-text search: to_tsvector(...) @@ to_tsquery(...)
                if *op == BinaryOperator::AtAt {
                    if let (Expr::Function(tsvector_func), Expr::Function(tsquery_func)) =
                        (left.as_ref(), right.as_ref())
                        && is_to_tsvector(tsvector_func)
                        && is_to_tsquery(tsquery_func)
                    {
                        return translate_fts_expression(tsvector_func, tsquery_func, schema);
                    }
                    // Unsupported @@ usage
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                        "The @@ operator is only supported for to_tsvector(...) @@ to_tsquery(...) \
                         full-text search expressions."
                            .to_string(),
                    ));
                }

                Expr::BinaryOp {
                    left: Box::new(left.translate(schema, options)?),
                    op: op.clone(),
                    right: Box::new(right.translate(schema, options)?),
                }
            }
            // Handle type casts (e.g., value::text)
            Expr::Cast { expr, data_type, format, kind, array } => {
                Expr::Cast {
                    expr: Box::new(expr.translate(schema, options)?),
                    data_type: data_type.translate(schema, options)?,
                    format: format.clone(),
                    kind: kind.clone(),
                    array: *array,
                }
            }
            // Handle NULL checks (IS NULL, IS NOT NULL)
            Expr::IsNull(inner) => Expr::IsNull(Box::new(inner.translate(schema, options)?)),
            Expr::IsNotNull(inner) => Expr::IsNotNull(Box::new(inner.translate(schema, options)?)),
            // Handle EXISTS subqueries
            Expr::Exists { subquery, negated } => {
                Expr::Exists {
                    subquery: Box::new(subquery.translate(schema, options)?),
                    negated: *negated,
                }
            }
            // Translate ILIKE to LIKE (SQLite LIKE is case-insensitive for ASCII)
            Expr::ILike { negated, any, expr, pattern, escape_char } => {
                Expr::Like {
                    negated: *negated,
                    any: *any,
                    expr: Box::new(expr.translate(schema, options)?),
                    pattern: Box::new(pattern.translate(schema, options)?),
                    escape_char: escape_char.clone(),
                }
            }
            // Pass through LIKE unchanged
            Expr::Like { negated, any, expr, pattern, escape_char } => {
                Expr::Like {
                    negated: *negated,
                    any: *any,
                    expr: Box::new(expr.translate(schema, options)?),
                    pattern: Box::new(pattern.translate(schema, options)?),
                    escape_char: escape_char.clone(),
                }
            }
            _ => {
                unimplemented!(
                    "Expr translation for definition `{:?}` is not yet implemented.",
                    self
                )
            }
        })
    }
}
