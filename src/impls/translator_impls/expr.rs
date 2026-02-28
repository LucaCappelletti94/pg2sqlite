//! Implementation of the [`Translator`] trait for the
//! `Expr` type.

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    AccessExpr, Array, BinaryOperator, CastKind, DataType, DateTimeField, Expr, Function,
    FunctionArg, FunctionArgExpr, FunctionArgumentList, FunctionArguments, GroupByExpr, Ident,
    JsonPathElem, ObjectName, ObjectNamePart, Query, Select, SelectFlavor, SelectItem, SetExpr,
    Subscript, TableAlias, TableAliasColumnDef, TableFactor, TableWithJoins, UnaryOperator, Value,
    ValueWithSpan, helpers::attached_token::AttachedToken,
};

#[cfg(test)]
use crate::impls::timezone::is_fixed_utc_offset;
use crate::{
    impls::{
        datetime_helpers::{
            DatePartKey, build_strftime_call, datetime_field_key, strftime_mapping_for_key,
        },
        function_helpers::{integer_literal, simple_function_expr},
        shared_helpers::function_argument_exprs,
        timezone::normalize_timezone_modifier_for_sqlite,
    },
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
    let key = datetime_field_key(field).ok_or_else(|| {
        crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "EXTRACT({field}) is not supported in SQLite. Supported fields: \
             YEAR, MONTH, DAY, HOUR, MINUTE, SECOND, DOW, DOY, EPOCH."
        ))
    })?;
    // WEEK uses ISO 8601 week numbering in PostgreSQL (Monday-based, week 1
    // contains the first Thursday). SQLite's strftime('%W') uses Sunday-based
    // week numbers and disagrees near year boundaries. No single strftime
    // format produces ISO week numbers; emit a clear error instead.
    if key == DatePartKey::Week {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
            "EXTRACT(WEEK) is not supported in SQLite. PostgreSQL uses ISO 8601 \
             week numbers (Monday-based) while SQLite's strftime('%W') uses \
             Sunday-based week numbers — they diverge near year boundaries. \
             To compute ISO week number manually: \
             CAST((CAST(strftime('%j', date(ts, 'weekday 1', '-6 days')) AS INTEGER) \
             - 1) / 7 + 1 AS INTEGER)"
                .to_string(),
        ));
    }

    let (format_str, cast_type) = strftime_mapping_for_key(key);

    // Build: CAST(strftime('format', expr) AS cast_type)
    let translated_expr = expr.translate(schema, options)?;
    let strftime_call = build_strftime_call(format_str, translated_expr, None);

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

/// Translate a TRIM expression to SQLite.
///
/// PostgreSQL supports `TRIM(LEADING 'x' FROM str)`, `TRIM(TRAILING 'x' FROM
/// str)`, and `TRIM(BOTH 'x' FROM str)`.  SQLite has no such syntax; the
/// equivalents are `LTRIM(str, 'x')`, `RTRIM(str, 'x')`, and `TRIM(str,
/// 'x')` respectively.
///
/// When no `trim_what` character is given the directional variants still map
/// to `LTRIM(str)` / `RTRIM(str)` / `TRIM(str)` (SQLite's built-ins trim
/// whitespace when called with one argument).
///
/// Plain `TRIM(str)` with no direction or character passes through unchanged.
fn translate_trim(
    expr: &Expr,
    trim_where: Option<sqlparser::ast::TrimWhereField>,
    trim_what: Option<&Expr>,
    trim_characters: Option<&[Expr]>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    use sqlparser::ast::TrimWhereField;

    let func_name = match trim_where {
        Some(TrimWhereField::Leading) => Some("LTRIM"),
        Some(TrimWhereField::Trailing) => Some("RTRIM"),
        Some(TrimWhereField::Both) => Some("TRIM"),
        None => None,
    };

    if let Some(name) = func_name {
        let translated_expr = expr.translate(schema, options)?;
        let char_arg = match trim_what {
            Some(e) => Some(e.translate(schema, options)?),
            // trim_characters is the pg-dialect "TRIM(LEADING FROM str USING chars)" form;
            // treat the first character expression as the trim set when present.
            None => {
                trim_characters
                    .and_then(|c| c.first())
                    .map(|e| e.translate(schema, options))
                    .transpose()?
            }
        };

        let args = if let Some(char_expr) = char_arg {
            vec![
                FunctionArg::Unnamed(FunctionArgExpr::Expr(translated_expr)),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(char_expr)),
            ]
        } else {
            vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(translated_expr))]
        };

        return Ok(Expr::Function(Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args,
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        }));
    }

    // Plain TRIM(str) or TRIM(str, chars) — pass through with translated parts.
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

/// Build a function call expression with unnamed positional arguments (no
/// window specification).
fn function_call(name: &str, args: Vec<Expr>) -> Expr {
    simple_function_expr(name, args, None)
}

/// Translate PostgreSQL IS DISTINCT FROM / IS NOT DISTINCT FROM semantics
/// using SQLite's native null-safe IS operator.
///
/// - `x IS DISTINCT FROM y`     → `NOT (x IS y)`  (null-safe inequality)
/// - `x IS NOT DISTINCT FROM y` → `x IS y`         (null-safe equality)
fn translate_distinct_comparison(
    left: &Expr,
    right: &Expr,
    is_not_distinct: bool,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let l = left.translate(schema, options)?;
    let r = right.translate(schema, options)?;
    // SQLite's `x IS y` is null-safe equality: treats NULL as equal to NULL.
    let is_expr = Expr::BinaryOp {
        left: Box::new(l),
        op: BinaryOperator::Custom("IS".to_string()),
        right: Box::new(r),
    };
    if is_not_distinct {
        // IS NOT DISTINCT FROM → x IS y
        Ok(is_expr)
    } else {
        // IS DISTINCT FROM → NOT (x IS y)
        Ok(Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr: Box::new(Expr::Nested(Box::new(is_expr))),
        })
    }
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
#[cfg(test)]
fn is_sqlite_fixed_offset(value: &str) -> bool {
    is_fixed_utc_offset(value)
}

/// Normalize PostgreSQL AT TIME ZONE literal names to SQLite datetime
/// modifiers.
fn normalize_at_time_zone_modifier(value: &str) -> Option<String> {
    normalize_timezone_modifier_for_sqlite(value)
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

fn negate_predicate(expr: Expr) -> Expr {
    Expr::UnaryOp { op: UnaryOperator::Not, expr: Box::new(Expr::Nested(Box::new(expr))) }
}

fn fold_predicates(predicates: Vec<Expr>, op: &BinaryOperator, empty_value: bool) -> Expr {
    let mut iter = predicates.into_iter();
    let Some(first) = iter.next() else {
        return boolean_literal(empty_value);
    };
    iter.fold(first, |acc, expr| {
        Expr::BinaryOp { left: Box::new(acc), op: op.clone(), right: Box::new(expr) }
    })
}

fn build_exists_over_subquery(
    translated_left: &Expr,
    compare_op: &BinaryOperator,
    translated_subquery: Query,
    negate_comparison: bool,
    negate_exists: bool,
) -> Expr {
    const DERIVED_ALIAS: &str = "__pg2sqlite_quantifier";
    const ITEM_ALIAS: &str = "__pg2sqlite_item";

    let derived_alias = Ident::new(DERIVED_ALIAS);
    let item_alias = Ident::new(ITEM_ALIAS);
    let item_ref = Expr::CompoundIdentifier(vec![derived_alias.clone(), item_alias.clone()]);
    let mut comparison = Expr::BinaryOp {
        left: Box::new(translated_left.clone()),
        op: compare_op.clone(),
        right: Box::new(item_ref),
    };
    if negate_comparison {
        comparison = negate_predicate(comparison);
    }

    Expr::Exists {
        subquery: Box::new(Query {
            with: None,
            body: Box::new(SetExpr::Select(Box::new(Select {
                select_token: AttachedToken::empty(),
                distinct: None,
                top: None,
                top_before_distinct: false,
                projection: vec![SelectItem::UnnamedExpr(integer_literal(1))],
                into: None,
                from: vec![TableWithJoins {
                    relation: TableFactor::Derived {
                        lateral: false,
                        subquery: Box::new(translated_subquery),
                        alias: Some(TableAlias {
                            explicit: false,
                            name: derived_alias,
                            columns: vec![TableAliasColumnDef::from_name(ITEM_ALIAS)],
                        }),
                        sample: None,
                    },
                    joins: Vec::new(),
                }],
                lateral_views: Vec::new(),
                prewhere: None,
                selection: Some(comparison),
                group_by: GroupByExpr::Expressions(Vec::new(), Vec::new()),
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
        negated: negate_exists,
    }
}

fn translate_any_operation(
    left: &Expr,
    compare_op: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_left = left.translate(schema, options)?;
    match right {
        Expr::Subquery(q) => {
            Ok(build_exists_over_subquery(
                &translated_left,
                compare_op,
                q.translate(schema, options)?,
                false,
                false,
            ))
        }
        Expr::Array(Array { elem, .. }) => {
            let predicates = elem
                .iter()
                .map(|expr| {
                    Ok(Expr::BinaryOp {
                        left: Box::new(translated_left.clone()),
                        op: compare_op.clone(),
                        right: Box::new(expr.translate(schema, options)?),
                    })
                })
                .collect::<Result<Vec<_>, crate::errors::Error>>()?;
            Ok(fold_predicates(predicates, &BinaryOperator::Or, false))
        }
        Expr::Tuple(exprs) => {
            let predicates = exprs
                .iter()
                .map(|expr| {
                    Ok(Expr::BinaryOp {
                        left: Box::new(translated_left.clone()),
                        op: compare_op.clone(),
                        right: Box::new(expr.translate(schema, options)?),
                    })
                })
                .collect::<Result<Vec<_>, crate::errors::Error>>()?;
            Ok(fold_predicates(predicates, &BinaryOperator::Or, false))
        }
        _ => Err(crate::errors::Error::UnsupportedSQLiteFeature(
            "ANY/SOME operator with non-subquery/non-array expressions is not supported in SQLite."
                .to_string(),
        )),
    }
}

fn translate_all_operation(
    left: &Expr,
    compare_op: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_left = left.translate(schema, options)?;
    match right {
        Expr::Subquery(q) => {
            Ok(build_exists_over_subquery(
                &translated_left,
                compare_op,
                q.translate(schema, options)?,
                true,
                true,
            ))
        }
        Expr::Array(Array { elem, .. }) => {
            let predicates = elem
                .iter()
                .map(|expr| {
                    Ok(Expr::BinaryOp {
                        left: Box::new(translated_left.clone()),
                        op: compare_op.clone(),
                        right: Box::new(expr.translate(schema, options)?),
                    })
                })
                .collect::<Result<Vec<_>, crate::errors::Error>>()?;
            Ok(fold_predicates(predicates, &BinaryOperator::And, true))
        }
        Expr::Tuple(exprs) => {
            let predicates = exprs
                .iter()
                .map(|expr| {
                    Ok(Expr::BinaryOp {
                        left: Box::new(translated_left.clone()),
                        op: compare_op.clone(),
                        right: Box::new(expr.translate(schema, options)?),
                    })
                })
                .collect::<Result<Vec<_>, crate::errors::Error>>()?;
            Ok(fold_predicates(predicates, &BinaryOperator::And, true))
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

    // PostgreSQL JSON path operators (#> and #>>) have no SQLite equivalent.
    if matches!(op, BinaryOperator::HashArrow | BinaryOperator::HashLongArrow) {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "PostgreSQL JSON path operator `{op}` has no SQLite equivalent; \
             use `->` / `->>` for single-key access"
        )));
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

fn translate_json_path(
    path: &sqlparser::ast::JsonPath,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::JsonPath, crate::errors::Error> {
    Ok(sqlparser::ast::JsonPath {
        path: path
            .path
            .iter()
            .map(|elem| {
                Ok(match elem {
                    JsonPathElem::Dot { key, quoted } => {
                        JsonPathElem::Dot { key: key.clone(), quoted: *quoted }
                    }
                    JsonPathElem::Bracket { key } => {
                        JsonPathElem::Bracket { key: key.translate(schema, options)? }
                    }
                })
            })
            .collect::<Result<Vec<_>, crate::errors::Error>>()?,
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
            // LIKE passes through as-is
            Expr::Like { negated, any, expr, pattern, escape_char } => {
                Expr::Like {
                    negated: *negated,
                    any: *any,
                    expr: Box::new(expr.translate(schema, options)?),
                    pattern: Box::new(pattern.translate(schema, options)?),
                    escape_char: escape_char.clone(),
                }
            }
            // ILIKE → lower(expr) LIKE lower(pattern) — correct regardless of case_sensitive_like
            // pragma
            Expr::ILike { negated, any, expr, pattern, escape_char } => {
                let translated_expr = expr.translate(schema, options)?;
                let translated_pattern = pattern.translate(schema, options)?;
                Expr::Like {
                    negated: *negated,
                    any: *any,
                    expr: Box::new(wrap_with_lower(translated_expr)),
                    pattern: Box::new(wrap_with_lower(translated_pattern)),
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
            // COLLATE expression - validate that the collation is a SQLite built-in.
            // SQLite only supports BINARY (default), NOCASE, and RTRIM.
            // PostgreSQL collation names (e.g. "en_US", "C", "pg_catalog.default") are invalid
            // in SQLite and will cause a runtime error if passed through.
            Expr::Collate { expr, collation } => {
                let collation_name = collation
                    .0
                    .last()
                    .and_then(|p| p.as_ident())
                    .map(|i| i.value.to_ascii_uppercase())
                    .unwrap_or_default();
                if !matches!(collation_name.as_str(), "BINARY" | "NOCASE" | "RTRIM") {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                        "COLLATE {collation_name} is not a valid SQLite collation. \
                         SQLite only supports BINARY (default), NOCASE, and RTRIM. \
                         PostgreSQL collation names must be dropped or replaced before \
                         translation."
                    )));
                }
                Expr::Collate {
                    expr: Box::new(expr.translate(schema, options)?),
                    collation: collation.clone(),
                }
            }
            // Interval expressions (e.g. INTERVAL '7 days') are not valid SQLite syntax.
            // SQLite uses date modifier strings like date('now', '-7 days') instead.
            // Passing INTERVAL through would produce SQL that errors at runtime in SQLite.
            Expr::Interval(interval) => {
                let field_str =
                    interval.leading_field.as_ref().map_or(String::new(), |f| format!(" {f}"));
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "INTERVAL{field_str} expressions are not supported in SQLite. \
                     Use SQLite date modifiers instead: \
                     date('now', '-7 days'), datetime('now', '+1 hour'), etc."
                )));
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
                translate_any_operation(left, compare_op, right, schema, options)?
            }
            // ALL operations: x op ALL(subquery)
            // SQLite doesn't support ALL directly, but some cases can be converted
            Expr::AllOp { left, compare_op, right } => {
                // x <> ALL(subquery) is equivalent to x NOT IN (subquery)
                if matches!(compare_op, BinaryOperator::NotEq) {
                    return translate_any_all_to_in(left, right, true, schema, options);
                }
                translate_all_operation(left, compare_op, right, schema, options)?
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
            // JSON path operators (->, ->>) - translate child expressions and keep
            // path operators intact in the AST.
            Expr::JsonAccess { value, path } => {
                Expr::JsonAccess {
                    value: Box::new(value.translate(schema, options)?),
                    path: translate_json_path(path, schema, options)?,
                }
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

/// Wraps `expr` in a SQLite `lower()` call: `lower(expr)`.
/// Used to implement ILIKE → `lower(expr) LIKE lower(pattern)`.
fn wrap_with_lower(expr: Expr) -> Expr {
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("lower"))]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
    })
}

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{
            AccessExpr, BinaryOperator, DateTimeField, Expr, Function, FunctionArg,
            FunctionArgExpr, FunctionArgOperator, FunctionArgumentList, FunctionArguments, Ident,
            JsonPathElem, ObjectName, ObjectNamePart, Subscript,
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
    #[allow(clippy::too_many_lines)]
    fn translate_extract_trim_any_and_unimplemented_branches() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        // EXTRACT(WEEK) now correctly errors: SQLite's %W is Sunday-based while
        // PostgreSQL uses ISO 8601 Monday-based week numbers.
        let week_err = translate_extract(
            &DateTimeField::Week(None),
            &parse_expr("created_at"),
            &schema,
            &options,
        )
        .expect_err("week extract must now return an error (ISO vs Sunday-based mismatch)");
        assert!(
            week_err.to_string().to_lowercase().contains("week"),
            "Error must mention WEEK: {week_err}"
        );

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

        // TRIM(LEADING 'x' FROM str) → LTRIM(str, 'x')
        let ltrimmed = translate_trim(
            &parse_expr("str"),
            Some(sqlparser::ast::TrimWhereField::Leading),
            Some(&parse_expr("'x'")),
            None,
            &schema,
            &options,
        )
        .expect("leading trim should translate");
        assert_eq!(ltrimmed.to_string(), "LTRIM(str, 'x')");

        // TRIM(TRAILING 'x' FROM str) → RTRIM(str, 'x')
        let rtrimmed = translate_trim(
            &parse_expr("str"),
            Some(sqlparser::ast::TrimWhereField::Trailing),
            Some(&parse_expr("'x'")),
            None,
            &schema,
            &options,
        )
        .expect("trailing trim should translate");
        assert_eq!(rtrimmed.to_string(), "RTRIM(str, 'x')");

        // TRIM(BOTH 'x' FROM str) → TRIM(str, 'x')
        let btrimmed = translate_trim(
            &parse_expr("str"),
            Some(sqlparser::ast::TrimWhereField::Both),
            Some(&parse_expr("'x'")),
            None,
            &schema,
            &options,
        )
        .expect("both trim should translate");
        assert_eq!(btrimmed.to_string(), "TRIM(str, 'x')");

        // TRIM(LEADING FROM str) — no char → LTRIM(str)
        let ltrim_no_char = translate_trim(
            &parse_expr("str"),
            Some(sqlparser::ast::TrimWhereField::Leading),
            None,
            None,
            &schema,
            &options,
        )
        .expect("leading trim without char should translate");
        assert_eq!(ltrim_no_char.to_string(), "LTRIM(str)");

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

    #[test]
    fn translate_json_access_expression() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let json_access = Expr::JsonAccess {
            value: Box::new(parse_expr("payload")),
            path: sqlparser::ast::JsonPath {
                path: vec![
                    JsonPathElem::Dot { key: "author".to_string(), quoted: false },
                    JsonPathElem::Bracket { key: parse_expr("'name'") },
                ],
            },
        };

        let translated =
            json_access.translate(&schema, &options).expect("json access should translate");
        let rendered = translated.to_string();
        assert!(
            rendered.contains("payload"),
            "expected translated value expression, got: {rendered}"
        );
        assert!(rendered.contains("author"), "expected translated JSON path, got: {rendered}");
    }
}
