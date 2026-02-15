//! Implementation of the [`Translator`] trait for the
//! `Expr` type.

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    AccessExpr, Array, BinaryOperator, CastKind, DataType, DateTimeField, Expr, Function,
    FunctionArg, FunctionArgExpr, FunctionArgumentList, FunctionArguments, Ident, ObjectName,
    ObjectNamePart, Query, Select, SelectFlavor, SelectItem, SetExpr, TableFactor, TableWithJoins,
    Value, ValueWithSpan, helpers::attached_token::AttachedToken,
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
                connect_by: Vec::new(),
                flavor: SelectFlavor::Standard,
                exclude: None,
                optimizer_hint: None,
                select_modifiers: None,
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

/// Translate PostgreSQL EXTRACT(field FROM expr) to SQLite
/// CAST(strftime(format, expr) AS INTEGER).
///
/// PostgreSQL's EXTRACT returns a numeric value, while SQLite's strftime
/// returns a string. We wrap the strftime call in CAST to maintain numeric
/// semantics.
fn translate_extract(
    field: &DateTimeField,
    expr: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    // Map PostgreSQL date/time fields to strftime format strings
    let format_str = match field {
        DateTimeField::Year | DateTimeField::Years => "%Y",
        DateTimeField::Month | DateTimeField::Months => "%m",
        DateTimeField::Day | DateTimeField::Days => "%d",
        DateTimeField::Hour | DateTimeField::Hours => "%H",
        DateTimeField::Minute | DateTimeField::Minutes => "%M",
        DateTimeField::Second | DateTimeField::Seconds => "%S",
        DateTimeField::Week(_) | DateTimeField::Weeks => "%W",
        DateTimeField::DayOfWeek => "%w",
        DateTimeField::DayOfYear => "%j",
        other => {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "EXTRACT({other}) is not supported in SQLite. Supported fields: \
                 YEAR, MONTH, DAY, HOUR, MINUTE, SECOND, WEEK, DOW, DOY."
            )));
        }
    };

    // Build: CAST(strftime('format', expr) AS INTEGER)
    let translated_expr = expr.translate(schema, options)?;

    let strftime_call = Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("strftime"))]),
        uses_odbc_syntax: false,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(ValueWithSpan {
                    value: Value::SingleQuotedString(format_str.to_string()),
                    span: sqlparser::tokenizer::Span::empty(),
                }))),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(translated_expr)),
            ],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
    });

    // Wrap in CAST(... AS INTEGER) to match PostgreSQL's numeric return type
    Ok(Expr::Cast {
        expr: Box::new(strftime_call),
        data_type: DataType::Integer(None),
        format: None,
        kind: CastKind::Cast,
        array: false,
    })
}

/// Translate PostgreSQL FLOOR(x) to SQLite-compatible expression.
///
/// SQLite doesn't have a native FLOOR function. We translate it to:
/// `CASE WHEN x >= 0 OR x = CAST(x AS INTEGER) THEN CAST(x AS INTEGER)
///       ELSE CAST(x AS INTEGER) - 1 END`
///
/// This handles both positive and negative numbers correctly:
/// - FLOOR(3.7) = 3 (truncate)
/// - FLOOR(-3.7) = -4 (round toward negative infinity)
fn translate_floor(
    expr: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_expr = expr.translate(schema, options)?;

    // Build CAST(x AS INTEGER)
    let cast_to_int = Expr::Cast {
        expr: Box::new(translated_expr.clone()),
        data_type: DataType::Integer(None),
        format: None,
        kind: CastKind::Cast,
        array: false,
    };

    // Build: CASE WHEN x >= 0 OR x = CAST(x AS INTEGER) THEN CAST(x AS INTEGER)
    //        ELSE CAST(x AS INTEGER) - 1 END
    Ok(Expr::Case {
        case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
        end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
        operand: None,
        conditions: vec![sqlparser::ast::CaseWhen {
            condition: Expr::BinaryOp {
                left: Box::new(Expr::BinaryOp {
                    left: Box::new(translated_expr.clone()),
                    op: BinaryOperator::GtEq,
                    right: Box::new(Expr::Value(ValueWithSpan {
                        value: Value::Number("0".to_string(), false),
                        span: sqlparser::tokenizer::Span::empty(),
                    })),
                }),
                op: BinaryOperator::Or,
                right: Box::new(Expr::BinaryOp {
                    left: Box::new(translated_expr.clone()),
                    op: BinaryOperator::Eq,
                    right: Box::new(cast_to_int.clone()),
                }),
            },
            result: cast_to_int.clone(),
        }],
        else_result: Some(Box::new(Expr::BinaryOp {
            left: Box::new(cast_to_int),
            op: BinaryOperator::Minus,
            right: Box::new(Expr::Value(ValueWithSpan {
                value: Value::Number("1".to_string(), false),
                span: sqlparser::tokenizer::Span::empty(),
            })),
        })),
    })
}

/// Translate PostgreSQL CEIL(x) to SQLite-compatible expression.
///
/// SQLite doesn't have a native CEIL function. We translate it to:
/// `CASE WHEN x <= 0 OR x = CAST(x AS INTEGER) THEN CAST(x AS INTEGER)
///       ELSE CAST(x AS INTEGER) + 1 END`
///
/// This handles both positive and negative numbers correctly:
/// - CEIL(3.2) = 4 (round up)
/// - CEIL(-3.2) = -3 (truncate toward zero)
fn translate_ceil(
    expr: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_expr = expr.translate(schema, options)?;

    // Build CAST(x AS INTEGER)
    let cast_to_int = Expr::Cast {
        expr: Box::new(translated_expr.clone()),
        data_type: DataType::Integer(None),
        format: None,
        kind: CastKind::Cast,
        array: false,
    };

    // Build: CASE WHEN x <= 0 OR x = CAST(x AS INTEGER) THEN CAST(x AS INTEGER)
    //        ELSE CAST(x AS INTEGER) + 1 END
    Ok(Expr::Case {
        case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
        end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
        operand: None,
        conditions: vec![sqlparser::ast::CaseWhen {
            condition: Expr::BinaryOp {
                left: Box::new(Expr::BinaryOp {
                    left: Box::new(translated_expr.clone()),
                    op: BinaryOperator::LtEq,
                    right: Box::new(Expr::Value(ValueWithSpan {
                        value: Value::Number("0".to_string(), false),
                        span: sqlparser::tokenizer::Span::empty(),
                    })),
                }),
                op: BinaryOperator::Or,
                right: Box::new(Expr::BinaryOp {
                    left: Box::new(translated_expr),
                    op: BinaryOperator::Eq,
                    right: Box::new(cast_to_int.clone()),
                }),
            },
            result: cast_to_int.clone(),
        }],
        else_result: Some(Box::new(Expr::BinaryOp {
            left: Box::new(cast_to_int),
            op: BinaryOperator::Plus,
            right: Box::new(Expr::Value(ValueWithSpan {
                value: Value::Number("1".to_string(), false),
                span: sqlparser::tokenizer::Span::empty(),
            })),
        })),
    })
}

/// Translate a CASE expression, recursively translating all sub-expressions.
fn translate_case(
    case_token: &AttachedToken,
    end_token: &AttachedToken,
    operand: Option<&Expr>,
    conditions: &[sqlparser::ast::CaseWhen],
    else_result: Option<&Expr>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    Ok(Expr::Case {
        case_token: case_token.clone(),
        end_token: end_token.clone(),
        operand: operand.map(|e| e.translate(schema, options).map(Box::new)).transpose()?,
        conditions: conditions
            .iter()
            .map(|cw| {
                Ok(sqlparser::ast::CaseWhen {
                    condition: cw.condition.translate(schema, options)?,
                    result: cw.result.translate(schema, options)?,
                })
            })
            .collect::<Result<Vec<_>, crate::errors::Error>>()?,
        else_result: else_result.map(|e| e.translate(schema, options).map(Box::new)).transpose()?,
    })
}

/// Translate PostgreSQL POSITION(substr IN str) to SQLite INSTR(str, substr).
///
/// Note that the argument order is reversed: POSITION searches for the first
/// argument within the second, while INSTR searches for the second argument
/// within the first.
fn translate_position(
    substr_expr: &Expr,
    in_expr: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_substr = substr_expr.translate(schema, options)?;
    let translated_in = in_expr.translate(schema, options)?;

    // Build: INSTR(str, substr) - note the reversed argument order
    Ok(Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("INSTR"))]),
        uses_odbc_syntax: false,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                FunctionArg::Unnamed(FunctionArgExpr::Expr(translated_in)),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(translated_substr)),
            ],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
    }))
}

/// Translate a TRIM expression, recursively translating all sub-expressions.
fn translate_trim(
    expr: &Expr,
    trim_where: Option<sqlparser::ast::TrimWhereField>,
    trim_what: Option<&Expr>,
    trim_characters: Option<&[Expr]>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    Ok(Expr::Trim {
        expr: Box::new(expr.translate(schema, options)?),
        trim_where,
        trim_what: trim_what.map(|e| e.translate(schema, options).map(Box::new)).transpose()?,
        trim_characters: trim_characters
            .map(|chars| {
                chars.iter().map(|e| e.translate(schema, options)).collect::<Result<Vec<_>, _>>()
            })
            .transpose()?,
    })
}

/// Translate a SUBSTRING expression to SQLite SUBSTR.
fn translate_substring(
    expr: &Expr,
    substring_from: Option<&Expr>,
    substring_for: Option<&Expr>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    Ok(Expr::Substring {
        expr: Box::new(expr.translate(schema, options)?),
        substring_from: substring_from
            .map(|e| e.translate(schema, options).map(Box::new))
            .transpose()?,
        substring_for: substring_for
            .map(|e| e.translate(schema, options).map(Box::new))
            .transpose()?,
        special: true,   // Use SUBSTR(expr, start, len) syntax
        shorthand: true, // Use SUBSTR name
    })
}

/// Check if a data type is a pgvector type (vector or halfvec).
fn is_vector_type(data_type: &DataType) -> bool {
    if let DataType::Custom(name, _) = data_type
        && let Some(ident) = name.0.first().and_then(|p| p.as_ident())
    {
        let type_name = ident.value.to_lowercase();
        return type_name == "vector" || type_name == "halfvec";
    }
    false
}

/// Translate a vector type cast (e.g., `'[1,2,3]'::vector`) to
/// `vec_f32('[1,2,3]')`.
fn translate_vector_cast(
    expr: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_expr = expr.translate(schema, options)?;

    Ok(Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("vec_f32"))]),
        uses_odbc_syntax: false,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(translated_expr))],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
    }))
}

/// Translate a pgvector distance operator to a sqlite-vec function call.
///
/// pgvector operators:
/// - `<->` (LtDashGt) - Euclidean distance -> vec_distance_L2(a, b)
/// - `<=>` (Spaceship) - Cosine distance -> vec_distance_cosine(a, b)
/// - `<~>` (Custom) - Hamming distance -> vec_distance_hamming(a, b)
///
/// # Performance Note
///
/// sqlite-vec v0.1.x performs brute-force search (O(n)), not indexed search.
/// ANN indexing is planned: <https://github.com/asg017/sqlite-vec/issues/25>
fn translate_vector_distance_op(
    left: &Expr,
    right: &Expr,
    function_name: &str,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_left = left.translate(schema, options)?;
    let translated_right = right.translate(schema, options)?;

    Ok(Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(function_name))]),
        uses_odbc_syntax: false,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                FunctionArg::Unnamed(FunctionArgExpr::Expr(translated_left)),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(translated_right)),
            ],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
    }))
}

/// Translate a binary operation expression.
fn translate_binary_op(
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    // Check for full-text search: to_tsvector(...) @@ to_tsquery(...)
    if *op == BinaryOperator::AtAt {
        if let (Expr::Function(tsvector_func), Expr::Function(tsquery_func)) = (left, right)
            && is_to_tsvector(tsvector_func)
            && is_to_tsquery(tsquery_func)
        {
            return translate_fts_expression(tsvector_func, tsquery_func, schema);
        }
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
            "The @@ operator is only supported for to_tsvector(...) @@ to_tsquery(...) \
             full-text search expressions."
                .to_string(),
        ));
    }

    // pgvector distance operators -> sqlite-vec functions
    // <-> (LtDashGt) - Euclidean distance
    if *op == BinaryOperator::LtDashGt {
        return translate_vector_distance_op(left, right, "vec_distance_L2", schema, options);
    }

    // <=> (Spaceship) - Cosine distance
    // Note: In MySQL this is the NULL-safe equality operator, but in pgvector it's
    // cosine distance
    if *op == BinaryOperator::Spaceship {
        return translate_vector_distance_op(left, right, "vec_distance_cosine", schema, options);
    }

    // Custom operators for pgvector
    if let BinaryOperator::Custom(op_str) = op {
        match op_str.as_str() {
            // <~> - Hamming distance (pgvector bit vectors)
            "<~>" => {
                return translate_vector_distance_op(
                    left,
                    right,
                    "vec_distance_hamming",
                    schema,
                    options,
                );
            }
            // <#> - Negative inner product (not supported in sqlite-vec)
            "<#>" => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "The <#> operator (negative inner product) is not supported by sqlite-vec. \
                     Consider using <-> (L2 distance) or <=> (cosine distance) instead."
                        .to_string(),
                ));
            }
            // <+> - L1/Manhattan distance (not supported in sqlite-vec)
            "<+>" => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "The <+> operator (L1/Manhattan distance) is not supported by sqlite-vec. \
                     Consider using <-> (L2 distance) or <=> (cosine distance) instead."
                        .to_string(),
                ));
            }
            // <%> - Jaccard distance (not supported in sqlite-vec)
            "<%>" => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "The <%> operator (Jaccard distance) is not supported by sqlite-vec. \
                     Consider using <~> (Hamming distance) for bit vectors instead."
                        .to_string(),
                ));
            }
            _ => {}
        }
    }

    Ok(Expr::BinaryOp {
        left: Box::new(left.translate(schema, options)?),
        op: op.clone(),
        right: Box::new(right.translate(schema, options)?),
    })
}

impl Translator for Expr {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Self;

    #[allow(clippy::too_many_lines)]
    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        Ok(match self {
            Expr::Function(func) => func.translate(schema, options)?,
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
                translate_binary_op(left, op, right, schema, options)?
            }
            // Handle type casts (e.g., value::text)
            Expr::Cast { expr, data_type, format, kind, array } => {
                // pgvector casts: '[1,2,3]'::vector -> vec_f32('[1,2,3]')
                if is_vector_type(data_type) {
                    return translate_vector_cast(expr, schema, options);
                }
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
            // Handle boolean checks (IS TRUE, IS FALSE, IS NOT TRUE, IS NOT FALSE)
            Expr::IsTrue(inner) => Expr::IsTrue(Box::new(inner.translate(schema, options)?)),
            Expr::IsNotTrue(inner) => Expr::IsNotTrue(Box::new(inner.translate(schema, options)?)),
            Expr::IsFalse(inner) => Expr::IsFalse(Box::new(inner.translate(schema, options)?)),
            Expr::IsNotFalse(inner) => {
                Expr::IsNotFalse(Box::new(inner.translate(schema, options)?))
            }
            // Handle EXISTS subqueries
            Expr::Exists { subquery, negated } => {
                Expr::Exists {
                    subquery: Box::new(subquery.translate(schema, options)?),
                    negated: *negated,
                }
            }
            // ILIKE translates to LIKE (SQLite LIKE is case-insensitive for ASCII)
            Expr::ILike { negated, any, expr, pattern, escape_char }
            | Expr::Like { negated, any, expr, pattern, escape_char } => {
                Expr::Like {
                    negated: *negated,
                    any: *any,
                    expr: Box::new(expr.translate(schema, options)?),
                    pattern: Box::new(pattern.translate(schema, options)?),
                    escape_char: escape_char.clone(),
                }
            }
            // IN list: x IN (1, 2, 3) - pass through with translated expressions
            Expr::InList { expr, list, negated } => {
                Expr::InList {
                    expr: Box::new(expr.translate(schema, options)?),
                    list: list
                        .iter()
                        .map(|e| e.translate(schema, options))
                        .collect::<Result<Vec<_>, _>>()?,
                    negated: *negated,
                }
            }
            // IN subquery: x IN (SELECT ...) - pass through with translated subquery
            Expr::InSubquery { expr, subquery, negated } => {
                Expr::InSubquery {
                    expr: Box::new(expr.translate(schema, options)?),
                    subquery: Box::new(subquery.translate(schema, options)?),
                    negated: *negated,
                }
            }
            // BETWEEN: x BETWEEN low AND high - pass through with translated expressions
            Expr::Between { expr, negated, low, high } => {
                Expr::Between {
                    expr: Box::new(expr.translate(schema, options)?),
                    negated: *negated,
                    low: Box::new(low.translate(schema, options)?),
                    high: Box::new(high.translate(schema, options)?),
                }
            }
            // CASE expression - pass through with translated expressions
            Expr::Case { case_token, end_token, operand, conditions, else_result } => {
                translate_case(
                    case_token,
                    end_token,
                    operand.as_deref(),
                    conditions,
                    else_result.as_deref(),
                    schema,
                    options,
                )?
            }
            // Scalar subquery: (SELECT ...) - pass through with translated query
            Expr::Subquery(query) => Expr::Subquery(Box::new(query.translate(schema, options)?)),
            // EXTRACT: translate to SQLite strftime()
            Expr::Extract { field, expr, .. } => translate_extract(field, expr, schema, options)?,
            // Tuple/row value expression: (a, b, c) - pass through with translated elements
            Expr::Tuple(exprs) => {
                Expr::Tuple(
                    exprs
                        .iter()
                        .map(|e| e.translate(schema, options))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            // Array expression: ARRAY[a, b, c] or [a, b, c] - pass through with translated elements
            Expr::Array(Array { elem, named }) => {
                Expr::Array(Array {
                    elem: elem
                        .iter()
                        .map(|e| e.translate(schema, options))
                        .collect::<Result<Vec<_>, _>>()?,
                    named: *named,
                })
            }
            // TRIM expression - pass through with translated parts
            Expr::Trim { expr, trim_where, trim_what, trim_characters } => {
                translate_trim(
                    expr,
                    *trim_where,
                    trim_what.as_deref(),
                    trim_characters.as_deref(),
                    schema,
                    options,
                )?
            }
            // CEIL expression - translate to SQLite CASE expression
            Expr::Ceil { expr, .. } => translate_ceil(expr, schema, options)?,
            // FLOOR expression - translate to SQLite CASE expression
            Expr::Floor { expr, .. } => translate_floor(expr, schema, options)?,
            // POSITION(substr IN str) -> INSTR(str, substr)
            // Note: SQLite's INSTR has arguments in reverse order
            Expr::Position { expr, r#in } => translate_position(expr, r#in, schema, options)?,
            // SUBSTRING(str FROM pos FOR len) -> SUBSTR(str, pos, len)
            Expr::Substring { expr, substring_from, substring_for, .. } => {
                translate_substring(
                    expr,
                    substring_from.as_deref(),
                    substring_for.as_deref(),
                    schema,
                    options,
                )?
            }
            // TypedString (e.g., TEXT 'value') - translate to CAST
            Expr::TypedString(typed_string) => {
                Expr::Cast {
                    expr: Box::new(Expr::Value(typed_string.value.clone())),
                    data_type: typed_string.data_type.clone(),
                    format: None,
                    kind: sqlparser::ast::CastKind::Cast,
                    array: false,
                }
            }
            // Prefixed string (e.g., N'value', X'value') - translate the inner value
            Expr::Prefixed { value, .. } => value.translate(schema, options)?,
            // COLLATE expression - pass through with translated expression
            Expr::Collate { expr, collation } => {
                Expr::Collate {
                    expr: Box::new(expr.translate(schema, options)?),
                    collation: collation.clone(),
                }
            }
            // Interval expression - translate value, keep fields as-is
            Expr::Interval(interval) => {
                Expr::Interval(sqlparser::ast::Interval {
                    value: Box::new(interval.value.translate(schema, options)?),
                    leading_field: interval.leading_field.clone(),
                    leading_precision: interval.leading_precision,
                    last_field: interval.last_field.clone(),
                    fractional_seconds_precision: interval.fractional_seconds_precision,
                })
            }
            // Qualified wildcard (e.g., table.*) - pass through as-is
            Expr::QualifiedWildcard(name, token) => {
                Expr::QualifiedWildcard(name.clone(), token.clone())
            }
            // RLIKE/REGEXP expression - pass through with translated expressions
            Expr::RLike { negated, expr, pattern, regexp } => {
                Expr::RLike {
                    negated: *negated,
                    expr: Box::new(expr.translate(schema, options)?),
                    pattern: Box::new(pattern.translate(schema, options)?),
                    regexp: *regexp,
                }
            }
            // Compound field access (e.g., value.field or value[0].field)
            Expr::CompoundFieldAccess { root, access_chain } => {
                let translated_chain = access_chain
                    .iter()
                    .map(|access| {
                        match access {
                            AccessExpr::Dot(expr) => {
                                Ok::<_, crate::errors::Error>(AccessExpr::Dot(
                                    expr.translate(schema, options)?,
                                ))
                            }
                            AccessExpr::Subscript(s) => Ok(AccessExpr::Subscript(s.clone())),
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Expr::CompoundFieldAccess {
                    root: Box::new(root.translate(schema, options)?),
                    access_chain: translated_chain,
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
