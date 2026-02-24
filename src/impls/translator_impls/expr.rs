//! Implementation of the [`Translator`] trait for the
//! `Expr` type.

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    AccessExpr, Array, BinaryOperator, CastKind, DataType, DateTimeField, Expr, Function,
    FunctionArg, FunctionArgExpr, FunctionArgumentList, FunctionArguments, Ident, ObjectName,
    ObjectNamePart, Query, Select, SelectFlavor, SelectItem, SetExpr, Subscript, TableFactor,
    TableWithJoins, Value, ValueWithSpan, helpers::attached_token::AttachedToken,
};

use crate::{
    impls::shared_helpers::function_argument_exprs,
    prelude::{Pg2SqliteOptions, Translator},
};

/// Extract column names from a function's arguments (recursively).
fn extract_columns_from_function(func: &Function) -> Vec<String> {
    function_argument_exprs(&func.args).into_iter().flat_map(extract_columns_from_expr).collect()
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
    // to_tsquery can have 1 or 2 args: to_tsquery('query') or to_tsquery('config',
    // 'query'). The query is always the last expression argument.
    for expr in function_argument_exprs(&func.args).into_iter().rev() {
        if let Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(s), .. }) = expr {
            return Some(s.clone());
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
#[allow(clippy::too_many_lines)]
fn translate_fts_expression(
    tsvector_func: &Function,
    tsquery_func: &Function,
    schema: &ParserDB,
) -> Result<Expr, crate::errors::Error> {
    let columns = extract_columns_from_function(tsvector_func);
    let table = schema
        .tables()
        .find(|table| {
            if columns.is_empty() {
                return false;
            }
            let table_columns: std::collections::HashSet<_> =
                table.columns(schema).map(|c| c.column_name().to_lowercase()).collect();
            columns.iter().all(|col| table_columns.contains(&col.to_lowercase()))
        })
        .ok_or_else(|| {
            crate::errors::Error::UnsupportedSQLiteFeature(
                "Could not determine table name from to_tsvector expression. \
                 Ensure the columns referenced exist in a table with a GIN/GiST index."
                    .to_string(),
            )
        })?;
    let table_name = table.table_name().to_string();

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
                    left: Box::new(Expr::Identifier(Ident::new(fts_table_name))),
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
    // Map PostgreSQL date/time fields to (strftime format, cast type).
    // SECOND uses %f (includes fractional seconds: "SS.SSS") and casts to REAL.
    // EPOCH uses %s (Unix timestamp seconds) and casts to REAL for PG
    // compatibility. All other fields return integer values.
    let (format_str, cast_type) = match field {
        DateTimeField::Year | DateTimeField::Years => ("%Y", DataType::Integer(None)),
        DateTimeField::Month | DateTimeField::Months => ("%m", DataType::Integer(None)),
        DateTimeField::Day | DateTimeField::Days => ("%d", DataType::Integer(None)),
        DateTimeField::Hour | DateTimeField::Hours => ("%H", DataType::Integer(None)),
        DateTimeField::Minute | DateTimeField::Minutes => ("%M", DataType::Integer(None)),
        // %f returns "SS.SSS" preserving fractional seconds
        DateTimeField::Second | DateTimeField::Seconds => ("%f", DataType::Real),
        DateTimeField::Week(_) | DateTimeField::Weeks => ("%W", DataType::Integer(None)),
        DateTimeField::DayOfWeek => ("%w", DataType::Integer(None)),
        DateTimeField::DayOfYear => ("%j", DataType::Integer(None)),
        // EPOCH: strftime('%s') gives integer seconds since Unix epoch.
        // Cast to REAL for consistency with PostgreSQL's float return type.
        DateTimeField::Epoch => ("%s", DataType::Real),
        other => {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "EXTRACT({other}) is not supported in SQLite. Supported fields: \
                 YEAR, MONTH, DAY, HOUR, MINUTE, SECOND, WEEK, DOW, DOY, EPOCH."
            )));
        }
    };

    // Build: CAST(strftime('format', expr) AS cast_type)
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

    Ok(Expr::Cast {
        expr: Box::new(strftime_call),
        data_type: cast_type,
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
                    left: Box::new(translated_expr),
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

/// Create a SQLite-compatible boolean literal expression.
fn boolean_literal(value: bool) -> Expr {
    Expr::Value(ValueWithSpan {
        value: Value::Boolean(value),
        span: sqlparser::tokenizer::Span::empty(),
    })
}

/// Create a SQLite-compatible integer literal expression.
fn integer_literal(value: i64) -> Expr {
    Expr::Value(ValueWithSpan {
        value: Value::Number(value.to_string(), false),
        span: sqlparser::tokenizer::Span::empty(),
    })
}

/// Build a function call expression with unnamed positional arguments.
fn function_call(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))]),
        uses_odbc_syntax: false,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: args.into_iter().map(FunctionArgExpr::Expr).map(FunctionArg::Unnamed).collect(),
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
    })
}

/// Translate PostgreSQL IS DISTINCT FROM / IS NOT DISTINCT FROM semantics
/// using a CASE expression compatible with SQLite.
fn translate_distinct_comparison(
    left: &Expr,
    right: &Expr,
    is_not_distinct: bool,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_left = left.translate(schema, options)?;
    let translated_right = right.translate(schema, options)?;

    let both_null = Expr::BinaryOp {
        left: Box::new(Expr::IsNull(Box::new(translated_left.clone()))),
        op: BinaryOperator::And,
        right: Box::new(Expr::IsNull(Box::new(translated_right.clone()))),
    };

    let one_null = Expr::BinaryOp {
        left: Box::new(Expr::IsNull(Box::new(translated_left.clone()))),
        op: BinaryOperator::Or,
        right: Box::new(Expr::IsNull(Box::new(translated_right.clone()))),
    };

    let non_null_comparison = Expr::BinaryOp {
        left: Box::new(translated_left),
        op: if is_not_distinct { BinaryOperator::Eq } else { BinaryOperator::NotEq },
        right: Box::new(translated_right),
    };

    let (both_null_result, one_null_result) =
        if is_not_distinct { (true, false) } else { (false, true) };

    Ok(Expr::Case {
        case_token: AttachedToken::empty(),
        end_token: AttachedToken::empty(),
        operand: None,
        conditions: vec![
            sqlparser::ast::CaseWhen {
                condition: both_null,
                result: boolean_literal(both_null_result),
            },
            sqlparser::ast::CaseWhen {
                condition: one_null,
                result: boolean_literal(one_null_result),
            },
        ],
        else_result: Some(Box::new(non_null_comparison)),
    })
}

/// Translate PostgreSQL OVERLAY(str PLACING repl FROM pos [FOR len]) to
/// SQLite substr/concatenation operations.
fn translate_overlay(
    expr: &Expr,
    overlay_what: &Expr,
    overlay_from: &Expr,
    overlay_for: Option<&Expr>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_expr = expr.translate(schema, options)?;
    let translated_overlay_what = overlay_what.translate(schema, options)?;
    let translated_overlay_from = overlay_from.translate(schema, options)?;

    let replacement_len = if let Some(overlay_for) = overlay_for {
        overlay_for.translate(schema, options)?
    } else {
        function_call("length", vec![translated_overlay_what.clone()])
    };

    let prefix = function_call(
        "substr",
        vec![
            translated_expr.clone(),
            integer_literal(1),
            Expr::BinaryOp {
                left: Box::new(translated_overlay_from.clone()),
                op: BinaryOperator::Minus,
                right: Box::new(integer_literal(1)),
            },
        ],
    );

    let suffix = function_call(
        "substr",
        vec![
            translated_expr,
            Expr::BinaryOp {
                left: Box::new(translated_overlay_from),
                op: BinaryOperator::Plus,
                right: Box::new(replacement_len),
            },
        ],
    );

    Ok(Expr::BinaryOp {
        left: Box::new(Expr::BinaryOp {
            left: Box::new(prefix),
            op: BinaryOperator::StringConcat,
            right: Box::new(translated_overlay_what),
        }),
        op: BinaryOperator::StringConcat,
        right: Box::new(suffix),
    })
}

/// Return true when value is a SQLite-supported fixed UTC offset (`+HH:MM`,
/// `-HH:MM`).
fn is_sqlite_fixed_offset(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 6 || (bytes[0] != b'+' && bytes[0] != b'-') || bytes[3] != b':' {
        return false;
    }
    if !bytes[1].is_ascii_digit()
        || !bytes[2].is_ascii_digit()
        || !bytes[4].is_ascii_digit()
        || !bytes[5].is_ascii_digit()
    {
        return false;
    }

    let hour = (bytes[1] - b'0') * 10 + (bytes[2] - b'0');
    let minute = (bytes[4] - b'0') * 10 + (bytes[5] - b'0');

    hour <= 23 && minute <= 59
}

/// Normalize PostgreSQL AT TIME ZONE literal names to SQLite datetime
/// modifiers.
fn normalize_at_time_zone_modifier(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();

    match lower.as_str() {
        "utc" | "gmt" | "z" => return Some("utc".to_string()),
        "local" | "localtime" => return Some("localtime".to_string()),
        _ => {}
    }

    if is_sqlite_fixed_offset(trimmed) {
        return Some(trimmed.to_string());
    }

    for prefix in ["utc", "gmt"] {
        if let Some(rest) = lower.strip_prefix(prefix)
            && is_sqlite_fixed_offset(rest)
        {
            return Some(rest.to_string());
        }
    }

    None
}

/// Translate PostgreSQL `expr AT TIME ZONE '...'` to SQLite
/// `datetime(expr, modifier)`.
fn translate_at_time_zone(
    timestamp: &Expr,
    time_zone: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_timestamp = timestamp.translate(schema, options)?;

    let modifier = match time_zone {
        Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(value), .. }) => {
            normalize_at_time_zone_modifier(value)
        }
        _ => None,
    }
    .ok_or_else(|| {
        crate::errors::Error::UnsupportedSQLiteFeature(
            "AT TIME ZONE supports only literal UTC/local or fixed offsets (+HH:MM/-HH:MM) in SQLite translation."
                .to_string(),
        )
    })?;

    Ok(function_call(
        "datetime",
        vec![
            translated_timestamp,
            Expr::Value(ValueWithSpan {
                value: Value::SingleQuotedString(modifier),
                span: sqlparser::tokenizer::Span::empty(),
            }),
        ],
    ))
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

/// Check if a data type is the halfvec (16-bit float vector) type specifically.
fn is_halfvec_type(data_type: &DataType) -> bool {
    if let DataType::Custom(name, _) = data_type
        && let Some(ident) = name.0.first().and_then(|p| p.as_ident())
    {
        return ident.value.to_lowercase() == "halfvec";
    }
    false
}

/// Translate a vector type cast to the appropriate sqlite-vec function.
///
/// - `'[1,2,3]'::vector` → `vec_f32('[1,2,3]')` (32-bit float)
/// - `'[1,2,3]'::halfvec` → `vec_f16('[1,2,3]')` (16-bit float)
fn translate_vector_cast(
    expr: &Expr,
    data_type: &DataType,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_expr = expr.translate(schema, options)?;
    let func_name = if is_halfvec_type(data_type) { "vec_f16" } else { "vec_f32" };

    Ok(Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(func_name))]),
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

/// Translate ANY/ALL right-hand side into an IN/NOT IN target.
///
/// Supported forms:
/// - subquery: `x = ANY(SELECT ...)` -> `x IN (SELECT ...)`
/// - array literal: `x = ANY(ARRAY[...])` -> `x IN (...)`
/// - tuple: `x = ANY((...))` -> `x IN (...)`
fn translate_any_all_to_in(
    left: &Expr,
    right: &Expr,
    negated: bool,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_left = left.translate(schema, options)?;

    match right {
        Expr::Subquery(q) => {
            Ok(Expr::InSubquery {
                expr: Box::new(translated_left),
                subquery: Box::new(q.translate(schema, options)?),
                negated,
            })
        }
        Expr::Array(Array { elem, .. }) => {
            Ok(Expr::InList {
                expr: Box::new(translated_left),
                list: elem
                    .iter()
                    .map(|e| e.translate(schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                negated,
            })
        }
        Expr::Tuple(exprs) => {
            Ok(Expr::InList {
                expr: Box::new(translated_left),
                list: exprs
                    .iter()
                    .map(|e| e.translate(schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
                negated,
            })
        }
        _ => Err(crate::errors::Error::UnsupportedSQLiteFeature(
            "ANY/ALL operator with non-subquery/non-array expressions is not supported in SQLite."
                .to_string(),
        )),
    }
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

fn translate_access_expr(
    access: &AccessExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<AccessExpr, crate::errors::Error> {
    Ok(match access {
        AccessExpr::Dot(expr) => AccessExpr::Dot(expr.translate(schema, options)?),
        AccessExpr::Subscript(subscript) => {
            AccessExpr::Subscript(match subscript {
                Subscript::Index { index } => {
                    Subscript::Index { index: index.translate(schema, options)? }
                }
                Subscript::Slice { lower_bound, upper_bound, stride } => {
                    Subscript::Slice {
                        lower_bound: lower_bound
                            .as_ref()
                            .map(|expr| expr.translate(schema, options))
                            .transpose()?,
                        upper_bound: upper_bound
                            .as_ref()
                            .map(|expr| expr.translate(schema, options))
                            .transpose()?,
                        stride: stride
                            .as_ref()
                            .map(|expr| expr.translate(schema, options))
                            .transpose()?,
                    }
                }
            })
        }
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
                // pgvector casts: '[1,2,3]'::vector -> vec_f32('[1,2,3]'),
                //                 '[1,2,3]'::halfvec -> vec_f16('[1,2,3]')
                if is_vector_type(data_type) {
                    return translate_vector_cast(expr, data_type, schema, options);
                }
                Expr::Cast {
                    expr: Box::new(expr.translate(schema, options)?),
                    data_type: data_type.translate(schema, options)?,
                    format: format.clone(),
                    kind: kind.clone(),
                    array: *array,
                }
            }
            // AT TIME ZONE expression - map to SQLite datetime(..., modifier)
            Expr::AtTimeZone { timestamp, time_zone } => {
                translate_at_time_zone(timestamp, time_zone, schema, options)?
            }
            // Handle NULL checks and UNKNOWN checks. UNKNOWN is rewritten as
            // a NULL check on the boolean expression result.
            Expr::IsNull(inner) | Expr::IsUnknown(inner) => {
                Expr::IsNull(Box::new(inner.translate(schema, options)?))
            }
            Expr::IsNotNull(inner) | Expr::IsNotUnknown(inner) => {
                Expr::IsNotNull(Box::new(inner.translate(schema, options)?))
            }
            // IS [NOT] DISTINCT FROM maps to explicit CASE expression for stable
            // null-aware equality semantics in SQLite.
            Expr::IsDistinctFrom(left, right) => {
                translate_distinct_comparison(left, right, false, schema, options)?
            }
            Expr::IsNotDistinctFrom(left, right) => {
                translate_distinct_comparison(left, right, true, schema, options)?
            }
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
            // OVERLAY(str PLACING repl FROM pos [FOR len]) ->
            // substr(str, 1, pos - 1) || repl || substr(str, pos + len)
            Expr::Overlay { expr, overlay_what, overlay_from, overlay_for } => {
                translate_overlay(
                    expr,
                    overlay_what,
                    overlay_from,
                    overlay_for.as_deref(),
                    schema,
                    options,
                )?
            }
            // TypedString (e.g., TEXT 'value') - translate to CAST
            Expr::TypedString(typed_string) => {
                Expr::Cast {
                    expr: Box::new(Expr::Value(typed_string.value.clone())),
                    data_type: typed_string.data_type.translate(schema, options)?,
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
                    .map(|access| translate_access_expr(access, schema, options))
                    .collect::<Result<Vec<_>, _>>()?;
                Expr::CompoundFieldAccess {
                    root: Box::new(root.translate(schema, options)?),
                    access_chain: translated_chain,
                }
            }
            // IS [NOT] NORMALIZED - PostgreSQL Unicode normalization check
            // SQLite doesn't have built-in Unicode normalization support
            Expr::IsNormalized { .. } => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "IS NORMALIZED (Unicode normalization check) is not supported in SQLite. \
                     Consider using application-level normalization with ICU or a similar library."
                        .to_string(),
                ));
            }
            // ANY/SOME operations: x op ANY(subquery)
            // SQLite doesn't support ANY/SOME directly, but some cases can be converted
            Expr::AnyOp { left, compare_op, right, .. } => {
                // x = ANY(subquery) is equivalent to x IN (subquery)
                if matches!(compare_op, BinaryOperator::Eq) {
                    return translate_any_all_to_in(left, right, false, schema, options);
                }
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "The ANY/SOME operator with {compare_op} is not supported in SQLite. \
                     Only '= ANY(subquery)' and '= ANY(ARRAY[...])' can be converted to IN."
                )));
            }
            // ALL operations: x op ALL(subquery)
            // SQLite doesn't support ALL directly, but some cases can be converted
            Expr::AllOp { left, compare_op, right } => {
                // x <> ALL(subquery) is equivalent to x NOT IN (subquery)
                if matches!(compare_op, BinaryOperator::NotEq) {
                    return translate_any_all_to_in(left, right, true, schema, options);
                }
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "The ALL operator with {compare_op} is not supported in SQLite. \
                     Only '<> ALL(subquery)' and '<> ALL(ARRAY[...])' can be converted to NOT IN."
                )));
            }
            // SIMILAR TO - SQL standard regex-like pattern matching
            // SQLite doesn't support SIMILAR TO; it only has LIKE and GLOB
            Expr::SimilarTo { .. } => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "SIMILAR TO is not supported in SQLite. \
                     Consider using LIKE for simple patterns or application-level regex matching."
                        .to_string(),
                ));
            }
            // ROLLUP is not supported in SQLite
            Expr::Rollup(_) => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "ROLLUP is not supported in SQLite. \
                     Restructure as separate GROUP BY queries and UNION ALL the results."
                        .to_string(),
                ));
            }
            // CUBE is not supported in SQLite
            Expr::Cube(_) => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "CUBE is not supported in SQLite. \
                     Restructure as separate GROUP BY queries and UNION ALL the results."
                        .to_string(),
                ));
            }
            // GROUPING SETS are not supported in SQLite
            Expr::GroupingSets(_) => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "GROUPING SETS are not supported in SQLite. \
                     Restructure as separate GROUP BY queries and UNION ALL the results."
                        .to_string(),
                ));
            }
            // IN UNNEST (PostgreSQL array unnesting) has no SQLite equivalent
            Expr::InUnnest { .. } => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "IN UNNEST (array unnesting) is not supported in SQLite. \
                     Use a subquery with a VALUES clause or a temporary table instead."
                        .to_string(),
                ));
            }
            // JSON path operators (->, ->>) are not yet translated
            Expr::JsonAccess { .. } => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "JSON path operators (->, ->>) are not yet translated. \
                     Use SQLite's json_extract(column, '$.field') instead."
                        .to_string(),
                ));
            }
            // Lambda expressions are not supported in SQLite
            Expr::Lambda(_) => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "Lambda expressions are not supported in SQLite.".to_string(),
                ));
            }
            // MATCH...AGAINST is MySQL syntax, not PostgreSQL
            Expr::MatchAgainst { .. } => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "MATCH...AGAINST is MySQL-specific syntax and is not supported in SQLite. \
                     Use SQLite FTS5: table MATCH 'query'."
                        .to_string(),
                ));
            }
            // Oracle OUTER JOIN (+) syntax
            Expr::OuterJoin(_) => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "Oracle outer join syntax (+) is not supported in SQLite. \
                     Use standard SQL LEFT JOIN / RIGHT JOIN syntax."
                        .to_string(),
                ));
            }
            // Oracle PRIOR expression (hierarchical queries)
            Expr::Prior(_) => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "PRIOR (Oracle hierarchical query syntax) is not supported in SQLite. \
                     Use recursive CTEs (WITH RECURSIVE) for hierarchical queries."
                        .to_string(),
                ));
            }
            _ => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "Expression translation not yet implemented: {self}"
                )));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{
            AccessExpr, BinaryOperator, DateTimeField, Expr, Function, FunctionArg,
            FunctionArgExpr, FunctionArgOperator, FunctionArgumentList, FunctionArguments, Ident,
            ObjectName, ObjectNamePart, Subscript,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        extract_columns_from_expr, extract_columns_from_function, extract_query_from_tsquery,
        is_sqlite_fixed_offset, normalize_at_time_zone_modifier, translate_any_all_to_in,
        translate_extract, translate_trim,
    };
    use crate::prelude::{Pg2SqliteOptions, Translator};

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build")
    }

    fn parse_expr(sql: &str) -> Expr {
        Parser::new(&PostgreSqlDialect {})
            .try_with_sql(sql)
            .expect("sql should parse")
            .parse_expr()
            .expect("expression should parse")
    }

    #[test]
    fn extract_helpers_cover_named_and_non_expr_argument_shapes() {
        let named_func = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("f"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![
                    FunctionArg::Named {
                        name: Ident::new("x"),
                        arg: FunctionArgExpr::Expr(parse_expr("tbl.col")),
                        operator: FunctionArgOperator::RightArrow,
                    },
                    FunctionArg::Unnamed(FunctionArgExpr::Wildcard),
                ],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        let cols = extract_columns_from_function(&named_func);
        assert_eq!(cols, vec!["col".to_string()]);

        let none_args_func = Function { args: FunctionArguments::None, ..named_func.clone() };
        assert!(extract_columns_from_function(&none_args_func).is_empty());

        assert!(extract_columns_from_expr(&Expr::CompoundIdentifier(Vec::new())).is_empty());
        assert_eq!(
            extract_columns_from_expr(&Expr::Nested(Box::new(parse_expr("a + b")))),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(
            extract_columns_from_expr(&Expr::Cast {
                expr: Box::new(parse_expr("payload")),
                data_type: sqlparser::ast::DataType::Text,
                format: None,
                kind: sqlparser::ast::CastKind::Cast,
                array: false,
            }),
            vec!["payload".to_string()]
        );
        assert_eq!(extract_columns_from_expr(&Expr::Function(named_func)), vec!["col".to_string()]);
    }

    #[test]
    fn tsquery_extract_and_timezone_helpers_cover_literal_and_invalid_paths() {
        let tsquery = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("to_tsquery"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Named {
                    name: Ident::new("query"),
                    arg: FunctionArgExpr::Expr(Expr::Value(sqlparser::ast::ValueWithSpan::from(
                        sqlparser::ast::Value::SingleQuotedString("a & b".to_string()),
                    ))),
                    operator: FunctionArgOperator::RightArrow,
                }],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        assert_eq!(extract_query_from_tsquery(&tsquery).as_deref(), Some("a & b"));

        let non_literal = Function {
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Named {
                    name: Ident::new("query"),
                    arg: FunctionArgExpr::Expr(parse_expr("param")),
                    operator: FunctionArgOperator::RightArrow,
                }],
                clauses: vec![],
            }),
            ..tsquery.clone()
        };
        assert!(extract_query_from_tsquery(&non_literal).is_none());

        assert!(!is_sqlite_fixed_offset("+0x:00"));
        assert_eq!(normalize_at_time_zone_modifier("utc+05:30").as_deref(), Some("+05:30"));
    }

    #[test]
    fn translate_extract_trim_any_and_unimplemented_branches() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        let extracted_week = translate_extract(
            &DateTimeField::Week(None),
            &parse_expr("created_at"),
            &schema,
            &options,
        )
        .expect("week extract should translate");
        assert!(extracted_week.to_string().contains("strftime('%W'"));

        let extracted_day_of_year = translate_extract(
            &DateTimeField::DayOfYear,
            &parse_expr("created_at"),
            &schema,
            &options,
        )
        .expect("doy extract should translate");
        assert!(extracted_day_of_year.to_string().contains("strftime('%j'"));

        let extracted_day_of_week = translate_extract(
            &DateTimeField::DayOfWeek,
            &parse_expr("created_at"),
            &schema,
            &options,
        )
        .expect("dow extract should translate");
        assert!(extracted_day_of_week.to_string().contains("strftime('%w'"));

        let trimmed = translate_trim(
            &parse_expr("body"),
            None,
            None,
            Some(&[parse_expr("'x'"), parse_expr("'y'")]),
            &schema,
            &options,
        )
        .expect("trim should translate");
        assert!(trimmed.to_string().contains("TRIM"));

        let in_tuple = translate_any_all_to_in(
            &parse_expr("id"),
            &Expr::Tuple(vec![parse_expr("1"), parse_expr("2")]),
            false,
            &schema,
            &options,
        )
        .expect("ANY tuple should become IN list");
        assert!(matches!(in_tuple, Expr::InList { .. }));

        let err = translate_any_all_to_in(
            &parse_expr("id"),
            &parse_expr("other_col"),
            false,
            &schema,
            &options,
        )
        .expect_err("unsupported ANY right expression should error");
        assert!(err.to_string().contains("ANY/ALL operator"));

        let access = Expr::CompoundFieldAccess {
            root: Box::new(parse_expr("payload")),
            access_chain: vec![
                AccessExpr::Subscript(Subscript::Index { index: parse_expr("1") }),
                AccessExpr::Dot(parse_expr("value")),
            ],
        };
        let translated_access =
            access.translate(&schema, &options).expect("access should translate");
        assert!(translated_access.to_string().contains("[1]"));

        let unsupported = Expr::InUnnest {
            expr: Box::new(parse_expr("id")),
            array_expr: Box::new(parse_expr("arr")),
            negated: false,
        };
        let err =
            unsupported.translate(&schema, &options).expect_err("unsupported expr should error");
        assert!(
            err.to_string().contains("not supported")
                || err.to_string().contains("not yet implemented")
        );

        let any_expr = Expr::AnyOp {
            left: Box::new(parse_expr("id")),
            compare_op: BinaryOperator::Eq,
            right: Box::new(Expr::Tuple(vec![parse_expr("1"), parse_expr("2")])),
            is_some: true,
        };
        let translated_any = any_expr.translate(&schema, &options).expect("ANY should translate");
        assert!(matches!(translated_any, Expr::InList { .. }));
    }
}
