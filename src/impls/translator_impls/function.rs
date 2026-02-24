//! Implementation of the [`Translator`] trait for the
//! `Function` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentList,
    FunctionArguments, Ident, ObjectName, Value, ValueWithSpan,
};

use crate::{
    prelude::{Pg2SqliteOptions, Translator},
    traits::TranslationOptions,
};

/// Represents a function translation result.
enum FunctionTranslation {
    /// Simple name replacement (e.g., LEAST -> MIN)
    Rename(String),
    /// Function with modified arguments (e.g., NOW() -> datetime('now'))
    WithArgs { name: String, args: Vec<FunctionArg> },
    /// Transform to concatenation operator (CONCAT -> ||)
    ToConcatenation,
    /// Transform to concatenation with separator (CONCAT_WS)
    ToConcatenationWithSeparator,
    /// Unsupported function with error message
    Unsupported(String),
    /// No translation needed
    PassThrough,
}

fn translate_function(
    name: &ObjectName,
    _args: &FunctionArguments,
    options: &Pg2SqliteOptions,
) -> FunctionTranslation {
    let original_name = name
        .0
        .last()
        .and_then(|part| part.as_ident())
        .map_or_else(|| name.to_string().to_lowercase(), |ident| {
            ident.value.to_ascii_lowercase()
        });

    match original_name.as_str() {
        "least" => FunctionTranslation::Rename("MIN".to_string()),
        "greatest" => FunctionTranslation::Rename("MAX".to_string()),
        "gen_random_uuid" | "uuid_generate_v4" | "uuidv4" | "uuidv7" => {
            FunctionTranslation::Rename(options.get_uuid_function_name().to_string())
        }
        "now" => {
            // NOW() -> datetime('now')
            FunctionTranslation::WithArgs {
                name: "datetime".to_string(),
                args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    ValueWithSpan {
                        value: Value::SingleQuotedString("now".to_string()),
                        span: sqlparser::tokenizer::Span::empty(),
                    },
                )))],
            }
        }
        // string_agg -> group_concat (for SQLite < 3.44 compatibility)
        "string_agg" => FunctionTranslation::Rename("group_concat".to_string()),
        "ts_rank" | "ts_rank_cd" => FunctionTranslation::Unsupported(
            "ts_rank/ts_rank_cd are not directly translatable to SQLite. \
             FTS5 provides bm25() for ranking, but it requires a different query structure. \
             Consider querying the FTS5 table directly: \
             SELECT *, bm25(table_fts) AS rank FROM table_fts WHERE table_fts MATCH 'query' ORDER BY rank"
                .to_string(),
        ),
        // CONCAT(a, b, c) -> a || b || c
        "concat" => FunctionTranslation::ToConcatenation,
        // CONCAT_WS(sep, a, b, c) -> a || sep || b || sep || c
        "concat_ws" => FunctionTranslation::ToConcatenationWithSeparator,
        // strpos(string, substring) -> INSTR(string, substring)
        "strpos" => FunctionTranslation::Rename("INSTR".to_string()),
        // chr(n) -> char(n)
        "chr" => FunctionTranslation::Rename("char".to_string()),
        _ => FunctionTranslation::PassThrough,
    }
}

/// Extract expressions from function arguments.
fn extract_arg_exprs(args: &FunctionArguments) -> Vec<&Expr> {
    match args {
        FunctionArguments::List(list) => {
            list.args
                .iter()
                .filter_map(|arg| {
                    match arg {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                        | FunctionArg::Named { arg: FunctionArgExpr::Expr(e), .. } => Some(e),
                        _ => None,
                    }
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Build a concatenation expression from a list of expressions using ||.
fn build_concatenation(exprs: Vec<Expr>) -> Option<Expr> {
    if exprs.is_empty() {
        return None;
    }
    if exprs.len() == 1 {
        return Some(exprs.into_iter().next().unwrap());
    }

    let mut iter = exprs.into_iter();
    let first = iter.next().unwrap();

    Some(iter.fold(first, |acc, expr| {
        Expr::BinaryOp {
            left: Box::new(acc),
            op: BinaryOperator::StringConcat,
            right: Box::new(expr),
        }
    }))
}

/// Build a concatenation expression with separator: first || sep || next...
fn build_concatenation_with_separator(separator: &Expr, first: Expr, remaining: Vec<Expr>) -> Expr {
    remaining.into_iter().fold(first, |acc, expr| {
        Expr::BinaryOp {
            left: Box::new(Expr::BinaryOp {
                left: Box::new(acc),
                op: BinaryOperator::StringConcat,
                right: Box::new(separator.clone()),
            }),
            op: BinaryOperator::StringConcat,
            right: Box::new(expr),
        }
    })
}

/// Wrap an aggregate function argument with CASE WHEN filter THEN value END.
///
/// This transforms `AGG(value) FILTER (WHERE condition)` to
/// `AGG(CASE WHEN condition THEN value END)`.
fn wrap_arg_with_case_filter(arg: &FunctionArg, filter: &Expr) -> FunctionArg {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Case {
                case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                operand: None,
                conditions: vec![sqlparser::ast::CaseWhen {
                    condition: filter.clone(),
                    result: expr.clone(),
                }],
                else_result: None,
            }))
        }
        FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => {
            // COUNT(*) FILTER (WHERE cond) -> SUM(CASE WHEN cond THEN 1 END)
            // But we can't change the function name here, so we wrap it differently
            // COUNT(*) FILTER -> COUNT(CASE WHEN cond THEN 1 END)
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Case {
                case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                operand: None,
                conditions: vec![sqlparser::ast::CaseWhen {
                    condition: filter.clone(),
                    result: Expr::Value(ValueWithSpan {
                        value: Value::Number("1".to_string(), false),
                        span: sqlparser::tokenizer::Span::empty(),
                    }),
                }],
                else_result: None,
            }))
        }
        FunctionArg::Named { name, arg: FunctionArgExpr::Expr(expr), operator } => {
            FunctionArg::Named {
                name: name.clone(),
                arg: FunctionArgExpr::Expr(Expr::Case {
                    case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                    end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                    operand: None,
                    conditions: vec![sqlparser::ast::CaseWhen {
                        condition: filter.clone(),
                        result: expr.clone(),
                    }],
                    else_result: None,
                }),
                operator: operator.clone(),
            }
        }
        // Pass through other argument types unchanged
        other => other.clone(),
    }
}

/// Transform a function with FILTER clause to use CASE expression instead.
fn transform_filter_to_case(func: &Function) -> Function {
    let filter = match &func.filter {
        Some(f) => f.as_ref(),
        None => return func.clone(),
    };

    let new_args = match &func.args {
        FunctionArguments::List(list) => {
            FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: list.duplicate_treatment,
                args: list.args.iter().map(|arg| wrap_arg_with_case_filter(arg, filter)).collect(),
                clauses: list.clauses.clone(),
            })
        }
        other => other.clone(),
    };

    Function {
        name: func.name.clone(),
        uses_odbc_syntax: func.uses_odbc_syntax,
        parameters: func.parameters.clone(),
        args: new_args,
        filter: None, // Remove the FILTER clause
        null_treatment: func.null_treatment,
        over: func.over.clone(),
        within_group: func.within_group.clone(),
    }
}

impl Translator for Function {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Expr;

    fn translate(
        &self,
        _schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // Transform FILTER clause to CASE expression
        let func =
            if self.filter.is_some() { transform_filter_to_case(self) } else { self.clone() };

        match translate_function(&func.name, &func.args, options) {
            FunctionTranslation::Rename(new_name) => {
                Ok(Expr::Function(Function {
                    name: ObjectName::from(vec![Ident::new(new_name)]),
                    uses_odbc_syntax: func.uses_odbc_syntax,
                    parameters: func.parameters.clone(),
                    args: func.args.clone(),
                    filter: None,
                    null_treatment: func.null_treatment,
                    over: func.over.clone(),
                    within_group: func.within_group.clone(),
                }))
            }
            FunctionTranslation::WithArgs { name, args } => {
                Ok(Expr::Function(Function {
                    name: ObjectName::from(vec![Ident::new(name)]),
                    uses_odbc_syntax: false,
                    parameters: FunctionArguments::None,
                    args: FunctionArguments::List(FunctionArgumentList {
                        duplicate_treatment: None,
                        args,
                        clauses: vec![],
                    }),
                    filter: None,
                    null_treatment: None,
                    over: None,
                    within_group: vec![],
                }))
            }
            FunctionTranslation::ToConcatenation => {
                // CONCAT(a, b, c) -> a || b || c
                let exprs: Vec<Expr> = extract_arg_exprs(&func.args).into_iter().cloned().collect();
                build_concatenation(exprs).ok_or_else(|| {
                    crate::errors::Error::UnsupportedSQLiteFeature(
                        "CONCAT requires at least one argument".to_string(),
                    )
                })
            }
            FunctionTranslation::ToConcatenationWithSeparator => {
                // CONCAT_WS(sep, a, b, c) -> a || sep || b || sep || c
                let mut exprs: Vec<Expr> =
                    extract_arg_exprs(&func.args).into_iter().cloned().collect();
                if exprs.len() < 2 {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                        "CONCAT_WS requires at least two arguments (separator and one value)"
                            .to_string(),
                    ));
                }
                let separator = exprs.remove(0);
                let first_value = exprs.remove(0);
                Ok(build_concatenation_with_separator(&separator, first_value, exprs))
            }
            FunctionTranslation::Unsupported(msg) => {
                Err(crate::errors::Error::UnsupportedSQLiteFeature(msg))
            }
            FunctionTranslation::PassThrough => Ok(Expr::Function(func)),
        }
    }
}

#[cfg(test)]
mod tests {
    use sqlparser::{
        ast::{
            Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgOperator,
            FunctionArgumentList, FunctionArguments, Ident, ObjectName, ObjectNamePart,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        build_concatenation_with_separator, extract_arg_exprs, transform_filter_to_case,
        wrap_arg_with_case_filter,
    };

    fn parse_expr(sql: &str) -> Expr {
        Parser::new(&PostgreSqlDialect {})
            .try_with_sql(sql)
            .expect("sql should parse")
            .parse_expr()
            .expect("expression should parse")
    }

    #[test]
    fn helper_functions_cover_none_args_passthrough_and_separator_builder() {
        assert!(extract_arg_exprs(&FunctionArguments::None).is_empty());

        let sep = parse_expr("','");
        let concatenated = build_concatenation_with_separator(
            &sep,
            parse_expr("a"),
            vec![parse_expr("b"), parse_expr("c")],
        );
        assert_eq!(concatenated.to_string(), "a || ',' || b || ',' || c");

        let wildcard_named = FunctionArg::Named {
            name: Ident::new("value"),
            arg: FunctionArgExpr::Wildcard,
            operator: FunctionArgOperator::RightArrow,
        };
        let wrapped = wrap_arg_with_case_filter(&wildcard_named, &parse_expr("1 = 1"));
        assert_eq!(wrapped, wildcard_named);

        let passthrough = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("sum"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(parse_expr("value")))],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };
        assert_eq!(transform_filter_to_case(&passthrough), passthrough);
    }
}
