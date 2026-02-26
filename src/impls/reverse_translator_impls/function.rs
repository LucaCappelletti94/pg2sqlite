//! Implementation of the [`crate::traits::ReverseTranslator`] trait for the
//! `Function` type.
//!
//! This module handles the reversal of SQLite functions to their PostgreSQL
//! equivalents.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, DateTimeField, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArgumentList, FunctionArguments, Ident, ObjectName, ObjectNamePart, TrimWhereField,
    Value, ValueWithSpan,
};

use super::helpers::{Reverse, reverse_translate_window_type};
use crate::{
    errors::Error,
    impls::shared_helpers::{translate_function_argument_clauses, translate_order_by_expr},
    prelude::Pg2SqliteOptions,
};

/// Represents a function reversal result.
pub enum FunctionReversal {
    /// Simple name replacement (e.g., MIN -> LEAST)
    Rename(String),
    /// Transform to NOW() function
    ToNow,
    /// Transform datetime(expr, modifier) to expr AT TIME ZONE '...'
    ToAtTimeZone(String),
    /// Transform to EXTRACT expression
    ToExtract(DateTimeField),
    /// Transform to POSITION expression (INSTR -> POSITION with arg swap)
    ToPosition,
    /// Transform to operator (vec_distance_L2 -> <->)
    ToOperator(BinaryOperator),
    /// Transform vec_f32 to vector cast
    ToVectorCast,
    /// Transform char to chr
    ToChr,
    /// Transform LTRIM/RTRIM/TRIM(str, chars) to TRIM(LEADING|TRAILING|BOTH
    /// chars FROM str)
    ToTrimDirectional(TrimWhereField),
    /// No translation needed
    PassThrough,
}

/// Parse a strftime format string and return the appropriate DateTimeField.
fn parse_strftime_format(format: &str) -> Option<DateTimeField> {
    match format {
        "%Y" => Some(DateTimeField::Year),
        "%m" => Some(DateTimeField::Month),
        "%d" => Some(DateTimeField::Day),
        "%H" => Some(DateTimeField::Hour),
        "%M" => Some(DateTimeField::Minute),
        "%S" => Some(DateTimeField::Second),
        "%W" => Some(DateTimeField::Week(None)),
        "%w" => Some(DateTimeField::DayOfWeek),
        "%j" => Some(DateTimeField::DayOfYear),
        _ => None,
    }
}

/// Return true when value is a fixed UTC offset in `+HH:MM` / `-HH:MM` format.
fn is_fixed_utc_offset(value: &str) -> bool {
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

/// Normalize SQLite datetime timezone modifiers to PostgreSQL AT TIME ZONE
/// literals.
fn normalize_datetime_timezone_modifier(modifier: &str) -> Option<String> {
    let trimmed = modifier.trim();
    let lower = trimmed.to_ascii_lowercase();

    match lower.as_str() {
        "utc" | "gmt" | "z" => return Some("UTC".to_string()),
        "local" | "localtime" => return Some("localtime".to_string()),
        _ => {}
    }

    if is_fixed_utc_offset(trimmed) {
        return Some(trimmed.to_string());
    }

    for prefix in ["utc", "gmt"] {
        if let Some(rest) = lower.strip_prefix(prefix)
            && is_fixed_utc_offset(rest)
        {
            return Some(rest.to_string());
        }
    }

    None
}

/// Determine how to reverse a SQLite function to PostgreSQL.
pub fn reverse_function(name: &ObjectName, args: &FunctionArguments) -> FunctionReversal {
    let func_name = name.0.last().and_then(|part| part.as_ident()).map_or_else(
        || name.to_string().to_ascii_lowercase(),
        |ident| ident.value.to_ascii_lowercase(),
    );

    match func_name.as_str() {
        // datetime('now') -> NOW()
        "datetime" => {
            if let FunctionArguments::List(list) = args
                && list.args.len() == 1
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    ValueWithSpan { value: Value::SingleQuotedString(s), .. },
                )))) = list.args.first()
                && s == "now"
            {
                return FunctionReversal::ToNow;
            }
            if let FunctionArguments::List(list) = args
                && list.args.len() == 2
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    ValueWithSpan { value: Value::SingleQuotedString(modifier), .. },
                )))) = list.args.get(1)
                && let Some(zone) = normalize_datetime_timezone_modifier(modifier)
            {
                return FunctionReversal::ToAtTimeZone(zone);
            }
            FunctionReversal::PassThrough
        }
        // strftime('%Y', expr) -> EXTRACT(YEAR FROM expr)
        "strftime" => {
            if let FunctionArguments::List(list) = args
                && list.args.len() == 2
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    ValueWithSpan { value: Value::SingleQuotedString(format), .. },
                )))) = list.args.first()
                && let Some(field) = parse_strftime_format(format)
            {
                return FunctionReversal::ToExtract(field);
            }
            FunctionReversal::PassThrough
        }
        // INSTR(str, substr) -> POSITION(substr IN str)
        "instr" => FunctionReversal::ToPosition,
        // group_concat -> string_agg
        "group_concat" => FunctionReversal::Rename("string_agg".to_string()),
        // json_group_array -> json_agg
        "json_group_array" => FunctionReversal::Rename("json_agg".to_string()),
        // json_group_object -> json_object_agg
        "json_group_object" => FunctionReversal::Rename("json_object_agg".to_string()),
        // min(a, b, ...) -> LEAST(a, b, ...)
        // Keep aggregate MIN(x) unchanged (single-arg form).
        "min" => {
            if let FunctionArguments::List(list) = args
                && list.args.len() > 1
            {
                return FunctionReversal::Rename("LEAST".to_string());
            }
            FunctionReversal::PassThrough
        }
        // max(a, b, ...) -> GREATEST(a, b, ...)
        // Keep aggregate MAX(x) unchanged (single-arg form).
        "max" => {
            if let FunctionArguments::List(list) = args
                && list.args.len() > 1
            {
                return FunctionReversal::Rename("GREATEST".to_string());
            }
            FunctionReversal::PassThrough
        }
        // LTRIM(str, chars) -> TRIM(LEADING chars FROM str)
        // Single-arg LTRIM(str) is valid PG; pass through.
        "ltrim" => {
            if let FunctionArguments::List(list) = args
                && list.args.len() == 2
            {
                return FunctionReversal::ToTrimDirectional(TrimWhereField::Leading);
            }
            FunctionReversal::PassThrough
        }
        // RTRIM(str, chars) -> TRIM(TRAILING chars FROM str)
        "rtrim" => {
            if let FunctionArguments::List(list) = args
                && list.args.len() == 2
            {
                return FunctionReversal::ToTrimDirectional(TrimWhereField::Trailing);
            }
            FunctionReversal::PassThrough
        }
        // TRIM(str, chars) -> TRIM(BOTH chars FROM str)
        // Single-arg TRIM(str) is identical in PG; pass through.
        "trim" => {
            if let FunctionArguments::List(list) = args
                && list.args.len() == 2
            {
                return FunctionReversal::ToTrimDirectional(TrimWhereField::Both);
            }
            FunctionReversal::PassThrough
        }
        // char(n) -> chr(n)
        "char" => FunctionReversal::ToChr,

        // vec_distance_L2(a, b) -> a <-> b
        "vec_distance_l2" => FunctionReversal::ToOperator(BinaryOperator::LtDashGt),
        // vec_distance_cosine(a, b) -> a <=> b
        "vec_distance_cosine" => FunctionReversal::ToOperator(BinaryOperator::Spaceship),
        // vec_distance_hamming(a, b) -> a <~> b
        "vec_distance_hamming" => {
            FunctionReversal::ToOperator(BinaryOperator::Custom("<~>".to_string()))
        }
        // vec_f32(expr) -> expr::vector
        "vec_f32" => FunctionReversal::ToVectorCast,

        _ => FunctionReversal::PassThrough,
    }
}

/// Reverse translate a SQLite function to PostgreSQL.
#[allow(clippy::too_many_lines)]
pub fn reverse_translate_function(
    func: &Function,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    match reverse_function(&func.name, &func.args) {
        FunctionReversal::Rename(new_name) => {
            Ok(Expr::Function(Function {
                name: ObjectName::from(vec![Ident::new(new_name)]),
                uses_odbc_syntax: func.uses_odbc_syntax,
                parameters: func.parameters.clone(),
                args: reverse_translate_function_args(&func.args, schema, options)?,
                filter: func
                    .filter
                    .as_ref()
                    .map(|f| {
                        crate::prelude::ReverseTranslator::reverse_translate(
                            f.as_ref(),
                            schema,
                            options,
                        )
                    })
                    .transpose()?
                    .map(Box::new),
                null_treatment: func.null_treatment,
                over: reverse_translate_window_type(func.over.as_ref(), schema, options)?,
                within_group: func.within_group.clone(),
            }))
        }
        FunctionReversal::ToNow => {
            // datetime('now') -> NOW()
            Ok(Expr::Function(Function {
                name: ObjectName::from(vec![Ident::new("NOW")]),
                uses_odbc_syntax: false,
                parameters: FunctionArguments::None,
                args: FunctionArguments::List(FunctionArgumentList {
                    duplicate_treatment: None,
                    args: vec![],
                    clauses: vec![],
                }),
                filter: None,
                null_treatment: None,
                over: None,
                within_group: vec![],
            }))
        }
        FunctionReversal::ToAtTimeZone(time_zone) => {
            let FunctionArguments::List(list) = &func.args else {
                debug_assert!(
                    false,
                    "reverse_function classified datetime as ToAtTimeZone without list args"
                );
                unreachable!(
                    "internal invariant violation in datetime AT TIME ZONE reverse translation"
                );
            };
            debug_assert_eq!(
                list.args.len(),
                2,
                "reverse_function classified datetime as ToAtTimeZone without exactly 2 args"
            );
            let timestamp_expr = extract_expr_from_arg(
                list.args
                    .first()
                    .expect("reverse_function must provide datetime timestamp argument"),
            )?;
            let reversed_timestamp = crate::prelude::ReverseTranslator::reverse_translate(
                timestamp_expr,
                schema,
                options,
            )?;

            Ok(Expr::AtTimeZone {
                timestamp: Box::new(reversed_timestamp),
                time_zone: Box::new(Expr::Value(ValueWithSpan {
                    value: Value::SingleQuotedString(time_zone),
                    span: sqlparser::tokenizer::Span::empty(),
                })),
            })
        }
        FunctionReversal::ToExtract(field) => {
            // strftime('%Y', expr) -> EXTRACT(YEAR FROM expr)
            if let FunctionArguments::List(list) = &func.args
                && list.args.len() == 2
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))) = list.args.get(1)
            {
                let reversed_expr =
                    crate::prelude::ReverseTranslator::reverse_translate(expr, schema, options)?;
                return Ok(Expr::Extract {
                    field,
                    syntax: sqlparser::ast::ExtractSyntax::From,
                    expr: Box::new(reversed_expr),
                });
            }
            Err(Error::UnsupportedSQLiteFeature(
                "Invalid strftime arguments for EXTRACT conversion".to_string(),
            ))
        }
        FunctionReversal::ToPosition => {
            // INSTR(str, substr) -> POSITION(substr IN str)
            if let FunctionArguments::List(list) = &func.args
                && list.args.len() == 2
            {
                let str_expr = extract_expr_from_arg(&list.args[0])?;
                let substr_expr = extract_expr_from_arg(&list.args[1])?;

                let reversed_str = crate::prelude::ReverseTranslator::reverse_translate(
                    str_expr, schema, options,
                )?;
                let reversed_substr = crate::prelude::ReverseTranslator::reverse_translate(
                    substr_expr,
                    schema,
                    options,
                )?;

                return Ok(Expr::Position {
                    expr: Box::new(reversed_substr),
                    r#in: Box::new(reversed_str),
                });
            }
            Err(Error::UnsupportedSQLiteFeature("INSTR requires exactly 2 arguments".to_string()))
        }
        FunctionReversal::ToOperator(op) => {
            // vec_distance_*(a, b) -> a <op> b
            if let FunctionArguments::List(list) = &func.args
                && list.args.len() == 2
            {
                let left_expr = extract_expr_from_arg(&list.args[0])?;
                let right_expr = extract_expr_from_arg(&list.args[1])?;

                let reversed_left = crate::prelude::ReverseTranslator::reverse_translate(
                    left_expr, schema, options,
                )?;
                let reversed_right = crate::prelude::ReverseTranslator::reverse_translate(
                    right_expr, schema, options,
                )?;

                return Ok(Expr::BinaryOp {
                    left: Box::new(reversed_left),
                    op,
                    right: Box::new(reversed_right),
                });
            }
            Err(Error::UnsupportedSQLiteFeature(
                "Vector distance function requires exactly 2 arguments".to_string(),
            ))
        }
        FunctionReversal::ToVectorCast => {
            // vec_f32(expr) -> expr::vector
            if let FunctionArguments::List(list) = &func.args
                && list.args.len() == 1
            {
                let expr = extract_expr_from_arg(&list.args[0])?;
                let reversed_expr =
                    crate::prelude::ReverseTranslator::reverse_translate(expr, schema, options)?;

                return Ok(Expr::Cast {
                    expr: Box::new(reversed_expr),
                    data_type: sqlparser::ast::DataType::Custom(
                        ObjectName(vec![ObjectNamePart::Identifier(Ident::new("vector"))]),
                        vec![],
                    ),
                    format: None,
                    kind: sqlparser::ast::CastKind::DoubleColon,
                    array: false,
                });
            }
            Err(Error::UnsupportedSQLiteFeature("vec_f32 requires exactly 1 argument".to_string()))
        }
        FunctionReversal::ToChr => {
            // char(n) -> chr(n)
            Ok(Expr::Function(Function {
                name: ObjectName::from(vec![Ident::new("chr")]),
                uses_odbc_syntax: func.uses_odbc_syntax,
                parameters: reverse_translate_function_args(&func.parameters, schema, options)?,
                args: reverse_translate_function_args(&func.args, schema, options)?,
                filter: None,
                null_treatment: func.null_treatment,
                over: reverse_translate_window_type(func.over.as_ref(), schema, options)?,
                within_group: func
                    .within_group
                    .iter()
                    .map(|e| translate_order_by_expr::<Reverse>(e, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            }))
        }
        FunctionReversal::ToTrimDirectional(field) => {
            // LTRIM/RTRIM(str, chars) or TRIM(str, chars)
            // -> TRIM(LEADING|TRAILING|BOTH chars FROM str)
            let FunctionArguments::List(list) = &func.args else {
                unreachable!("reverse_function only returns ToTrimDirectional for List args");
            };
            debug_assert_eq!(list.args.len(), 2, "ToTrimDirectional requires exactly 2 args");
            let str_expr = extract_expr_from_arg(&list.args[0])?;
            let char_expr = extract_expr_from_arg(&list.args[1])?;
            let reversed_str =
                crate::prelude::ReverseTranslator::reverse_translate(str_expr, schema, options)?;
            let reversed_char =
                crate::prelude::ReverseTranslator::reverse_translate(char_expr, schema, options)?;
            Ok(Expr::Trim {
                expr: Box::new(reversed_str),
                trim_where: Some(field),
                trim_what: Some(Box::new(reversed_char)),
                trim_characters: None,
            })
        }
        FunctionReversal::PassThrough => {
            Ok(Expr::Function(Function {
                name: func.name.clone(),
                uses_odbc_syntax: func.uses_odbc_syntax,
                parameters: reverse_translate_function_args(&func.parameters, schema, options)?,
                args: reverse_translate_function_args(&func.args, schema, options)?,
                filter: func
                    .filter
                    .as_ref()
                    .map(|f| {
                        crate::prelude::ReverseTranslator::reverse_translate(
                            f.as_ref(),
                            schema,
                            options,
                        )
                    })
                    .transpose()?
                    .map(Box::new),
                null_treatment: func.null_treatment,
                over: reverse_translate_window_type(func.over.as_ref(), schema, options)?,
                within_group: func
                    .within_group
                    .iter()
                    .map(|e| translate_order_by_expr::<Reverse>(e, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            }))
        }
    }
}

/// Extract an expression from a function argument.
fn extract_expr_from_arg(arg: &FunctionArg) -> Result<&Expr, Error> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
        | FunctionArg::Named { arg: FunctionArgExpr::Expr(e), .. }
        | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(e), .. } => Ok(e),
        _ => {
            Err(Error::UnsupportedSQLiteFeature(
                "Expected expression argument in function".to_string(),
            ))
        }
    }
}

/// Reverse translate function arguments.
fn reverse_translate_function_args(
    args: &FunctionArguments,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<FunctionArguments, Error> {
    match args {
        FunctionArguments::List(list) => {
            let translated_args = list
                .args
                .iter()
                .map(|arg| reverse_translate_function_arg(arg, schema, options))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: list.duplicate_treatment,
                args: translated_args,
                clauses: translate_function_argument_clauses::<Reverse>(
                    &list.clauses,
                    schema,
                    options,
                )?,
            }))
        }
        FunctionArguments::Subquery(query) => {
            Ok(FunctionArguments::Subquery(Box::new(
                crate::prelude::ReverseTranslator::reverse_translate(
                    query.as_ref(),
                    schema,
                    options,
                )?,
            )))
        }
        FunctionArguments::None => Ok(FunctionArguments::None),
    }
}

/// Reverse translate a single function argument.
fn reverse_translate_function_arg(
    arg: &FunctionArg,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<FunctionArg, Error> {
    Ok(match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(
                crate::prelude::ReverseTranslator::reverse_translate(e, schema, options)?,
            ))
        }
        FunctionArg::Named { name, arg: FunctionArgExpr::Expr(e), operator } => {
            FunctionArg::Named {
                name: name.clone(),
                arg: FunctionArgExpr::Expr(crate::prelude::ReverseTranslator::reverse_translate(
                    e, schema, options,
                )?),
                operator: operator.clone(),
            }
        }
        FunctionArg::ExprNamed { name, arg: FunctionArgExpr::Expr(e), operator } => {
            FunctionArg::ExprNamed {
                name: crate::prelude::ReverseTranslator::reverse_translate(name, schema, options)?,
                arg: FunctionArgExpr::Expr(crate::prelude::ReverseTranslator::reverse_translate(
                    e, schema, options,
                )?),
                operator: operator.clone(),
            }
        }
        // Pass through wildcards and other arg types
        other => other.clone(),
    })
}

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{
            Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgOperator,
            FunctionArgumentList, FunctionArguments, Ident, ObjectName, ObjectNamePart,
            ValueWithSpan,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{extract_expr_from_arg, is_fixed_utc_offset, reverse_translate_function};
    use crate::prelude::Pg2SqliteOptions;

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

    fn parse_query(sql: &str) -> sqlparser::ast::Query {
        let stmt = Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("query SQL should parse");
        let stmt = stmt.into_iter().next().expect("query SQL should produce one statement");
        match stmt {
            sqlparser::ast::Statement::Query(query) => *query,
            other => panic!("expected query, got: {other:?}"),
        }
    }

    #[test]
    fn fixed_offset_validator_rejects_non_digit_offsets() {
        assert!(!is_fixed_utc_offset("+0x:00"));
        assert!(!is_fixed_utc_offset("+24:00"));
        assert!(is_fixed_utc_offset("+23:59"));
    }

    #[test]
    fn extract_expr_and_datetime_reverse_translation_reject_wildcards() {
        let wildcard = FunctionArg::Unnamed(FunctionArgExpr::Wildcard);
        let err = extract_expr_from_arg(&wildcard).expect_err("wildcard should be rejected");
        assert!(err.to_string().contains("Expected expression argument"));

        let func = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("datetime"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![
                    FunctionArg::Unnamed(FunctionArgExpr::Wildcard),
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(ValueWithSpan::from(
                        sqlparser::ast::Value::SingleQuotedString("utc".to_string()),
                    )))),
                ],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };

        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let err = reverse_translate_function(&func, &schema, &options)
            .expect_err("datetime wildcard argument should be rejected");
        assert!(err.to_string().contains("Expected expression argument"));
    }

    #[test]
    fn passthrough_reverse_translation_preserves_filter_expression() {
        let func = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("count"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Unnamed(FunctionArgExpr::Wildcard)],
                clauses: vec![],
            }),
            filter: Some(Box::new(parse_expr("age > 18"))),
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };

        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let translated = reverse_translate_function(&func, &schema, &options)
            .expect("count should pass through");
        let Expr::Function(function) = translated else {
            panic!("expected translated function");
        };

        assert_eq!(function.name.to_string(), "count");
        assert_eq!(function.filter.as_ref().map(ToString::to_string), Some("age > 18".to_string()));
    }

    #[test]
    fn extract_expr_from_arg_accepts_expr_named() {
        let arg = FunctionArg::ExprNamed {
            name: parse_expr("param"),
            arg: FunctionArgExpr::Expr(parse_expr("value")),
            operator: FunctionArgOperator::Equals,
        };
        let extracted = extract_expr_from_arg(&arg).expect("expr-named args should be supported");
        assert_eq!(extracted.to_string(), "value");
    }

    #[test]
    fn passthrough_reverse_translation_translates_expr_named_argument_expression() {
        let func = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("custom_fn"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::ExprNamed {
                    name: parse_expr("tz"),
                    arg: FunctionArgExpr::Expr(parse_expr("datetime('now')")),
                    operator: FunctionArgOperator::Equals,
                }],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };

        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let translated = reverse_translate_function(&func, &schema, &options)
            .expect("custom function should pass through with translated args");
        let Expr::Function(function) = translated else {
            panic!("expected translated function");
        };

        let arg = function.args.to_string();
        assert!(arg.contains("NOW()"), "expected expr-named arg to be reverse translated: {arg}");
    }

    #[test]
    fn ltrim_two_arg_reverses_to_trim_leading() {
        let func = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("LTRIM"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(parse_expr("str"))),
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(parse_expr("'x'"))),
                ],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let result = reverse_translate_function(&func, &schema, &options)
            .expect("LTRIM(str, 'x') should reverse-translate");
        assert_eq!(result.to_string(), "TRIM(LEADING 'x' FROM str)");
    }

    #[test]
    fn rtrim_two_arg_reverses_to_trim_trailing() {
        let func = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("RTRIM"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(parse_expr("str"))),
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(parse_expr("'x'"))),
                ],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let result = reverse_translate_function(&func, &schema, &options)
            .expect("RTRIM(str, 'x') should reverse-translate");
        assert_eq!(result.to_string(), "TRIM(TRAILING 'x' FROM str)");
    }

    #[test]
    fn trim_two_arg_reverses_to_trim_both() {
        let func = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("TRIM"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(parse_expr("str"))),
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(parse_expr("'x'"))),
                ],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let result = reverse_translate_function(&func, &schema, &options)
            .expect("TRIM(str, 'x') should reverse-translate");
        assert_eq!(result.to_string(), "TRIM(BOTH 'x' FROM str)");
    }

    #[test]
    fn ltrim_one_arg_passes_through() {
        let func = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("LTRIM"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(parse_expr("str")))],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let result = reverse_translate_function(&func, &schema, &options)
            .expect("LTRIM(str) should pass through");
        assert_eq!(result.to_string(), "LTRIM(str)");
    }

    #[test]
    fn passthrough_reverse_translation_translates_subquery_arguments() {
        let func = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("custom_fn"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::Subquery(Box::new(parse_query("SELECT datetime('now')"))),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };

        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let translated = reverse_translate_function(&func, &schema, &options)
            .expect("custom function with subquery args should reverse-translate");
        let Expr::Function(function) = translated else {
            panic!("expected translated function");
        };

        let arg = function.args.to_string();
        assert!(
            arg.contains("NOW()"),
            "expected subquery argument to be recursively reverse translated: {arg}"
        );
    }
}
