//! Implementation of the [`ReverseTranslator`] trait for the
//! `Function` type.
//!
//! This module handles the reversal of SQLite functions to their PostgreSQL
//! equivalents.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, DateTimeField, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArgumentList, FunctionArguments, Ident, ObjectName, ObjectNamePart, Value,
    ValueWithSpan,
};

use crate::{errors::Error, prelude::Pg2SqliteOptions};

/// Represents a function reversal result.
pub enum FunctionReversal {
    /// Simple name replacement (e.g., MIN -> LEAST)
    Rename(String),
    /// Transform to NOW() function
    ToNow,
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

/// Determine how to reverse a SQLite function to PostgreSQL.
pub fn reverse_function(name: &ObjectName, args: &FunctionArguments) -> FunctionReversal {
    let func_name = name.to_string().to_lowercase();

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
        // char(n) -> chr(n)
        "char" => FunctionReversal::ToChr,
        // MIN -> LEAST (when used as scalar, not aggregate)
        // We can't easily distinguish, so we leave this as PassThrough
        // and let the user handle context-sensitive cases

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
                over: func.over.clone(),
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
                parameters: func.parameters.clone(),
                args: reverse_translate_function_args(&func.args, schema, options)?,
                filter: None,
                null_treatment: func.null_treatment,
                over: func.over.clone(),
                within_group: func.within_group.clone(),
            }))
        }
        FunctionReversal::PassThrough => {
            Ok(Expr::Function(Function {
                name: func.name.clone(),
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
                over: func.over.clone(),
                within_group: func.within_group.clone(),
            }))
        }
    }
}

/// Extract an expression from a function argument.
fn extract_expr_from_arg(arg: &FunctionArg) -> Result<&Expr, Error> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
        | FunctionArg::Named { arg: FunctionArgExpr::Expr(e), .. } => Ok(e),
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
                clauses: list.clauses.clone(),
            }))
        }
        FunctionArguments::None | FunctionArguments::Subquery(_) => Ok(args.clone()),
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
        // Pass through wildcards and other arg types
        other => other.clone(),
    })
}
