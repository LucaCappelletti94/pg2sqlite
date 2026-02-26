//! Implementation of the [`Translator`] trait for the
//! `Function` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, CastKind, DataType, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArgumentList, FunctionArguments, Ident, ObjectName, ObjectNamePart, Value,
    ValueWithSpan,
};

use super::helpers::translate_window_type;
use crate::{
    impls::shared_helpers::{
        GENERATE_SERIES_UNSUPPORTED_MESSAGE, function_argument_exprs,
        translate_function_argument_clauses,
    },
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
    /// Transform date_trunc to strftime equivalent
    DateTrunc,
    /// Transform date_part('field', expr) to CAST(strftime(format, expr) AS
    /// type)
    DatePart,
    /// Transform to_char(expr, format) to strftime(mapped_format, expr)
    ToChar,
    /// Unsupported function with error message
    Unsupported(String),
    /// No translation needed
    PassThrough,
}

#[allow(clippy::too_many_lines)]
fn translate_function(
    name: &ObjectName,
    _args: &FunctionArguments,
    options: &Pg2SqliteOptions,
) -> FunctionTranslation {
    let original_name = name
        .0
        .last()
        .and_then(|part| part.as_ident())
        .map_or_else(|| name.to_string().to_lowercase(), |ident| ident.value.to_ascii_lowercase());

    match original_name.as_str() {
        // MIN/MAX mappings
        "least" => FunctionTranslation::Rename("MIN".to_string()),
        "greatest" => FunctionTranslation::Rename("MAX".to_string()),
        // bool_and / bool_or / every: these aggregate boolean values with NULL semantics
        // that differ from MIN/MAX (NULL inputs are ignored, not propagated). There is
        // no correct SQLite equivalent. Callers can rewrite as:
        //   bool_and(col)  →  MIN(CASE WHEN col THEN 1 ELSE 0 END) = 1
        //   bool_or(col)   →  MAX(CASE WHEN col THEN 1 ELSE 0 END) = 1
        "bool_and" | "every" => FunctionTranslation::Unsupported(
            "bool_and/every is not supported in SQLite. \
             Rewrite as: MIN(CASE WHEN col THEN 1 ELSE 0 END) = 1"
                .to_string(),
        ),
        "bool_or" => FunctionTranslation::Unsupported(
            "bool_or is not supported in SQLite. \
             Rewrite as: MAX(CASE WHEN col THEN 1 ELSE 0 END) = 1"
                .to_string(),
        ),
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
        // CONCAT(a, b, c) -> COALESCE(a, '') || COALESCE(b, '') || COALESCE(c, '')
        "concat" => FunctionTranslation::ToConcatenation,
        // CONCAT_WS(sep, a, b, c) -> COALESCE(a, '') || sep || COALESCE(b, '') || sep || COALESCE(c, '')
        "concat_ws" => FunctionTranslation::ToConcatenationWithSeparator,
        // strpos(string, substring) -> INSTR(string, substring)
        "strpos" => FunctionTranslation::Rename("INSTR".to_string()),
        // chr(n) -> char(n)
        "chr" => FunctionTranslation::Rename("char".to_string()),
        // char_length / character_length -> length (SQLite counts characters)
        "char_length" | "character_length" => FunctionTranslation::Rename("length".to_string()),
        // date_trunc(field, ts) -> strftime(format, ts)
        "date_trunc" => FunctionTranslation::DateTrunc,
        // array_agg has no SQLite equivalent (no native arrays)
        "array_agg" => FunctionTranslation::Unsupported(
            "array_agg is not supported in SQLite because arrays are not a native type. \
             Use group_concat() instead: GROUP_CONCAT(column, ',')"
                .to_string(),
        ),
        // json_agg / jsonb_agg -> json_group_array (SQLite JSON extension)
        "json_agg" | "jsonb_agg" => FunctionTranslation::Rename("json_group_array".to_string()),
        // json_object_agg / jsonb_object_agg -> json_group_object
        "json_object_agg" | "jsonb_object_agg" => {
            FunctionTranslation::Rename("json_group_object".to_string())
        }
        // bit_and / bit_or: no built-in SQLite aggregate equivalent
        "bit_and" => FunctionTranslation::Unsupported(
            "bit_and is not supported as an aggregate in SQLite. \
             Consider loading a custom extension or rewriting with bitwise expressions."
                .to_string(),
        ),
        "bit_or" => FunctionTranslation::Unsupported(
            "bit_or is not supported as an aggregate in SQLite. \
             Consider loading a custom extension or rewriting with bitwise expressions."
                .to_string(),
        ),
        // Statistical aggregates: not available in SQLite without an extension
        "stddev" | "stddev_pop" | "stddev_samp" => FunctionTranslation::Unsupported(
            "stddev/stddev_pop/stddev_samp are not supported in SQLite. \
             Consider loading the statistics1 extension or computing manually."
                .to_string(),
        ),
        "variance" | "var_pop" | "var_samp" => FunctionTranslation::Unsupported(
            "variance/var_pop/var_samp are not supported in SQLite. \
             Consider loading the statistics1 extension or computing manually."
                .to_string(),
        ),
        "corr" => FunctionTranslation::Unsupported(
            "corr (correlation) is not supported in SQLite. \
             Consider loading the statistics1 extension or computing manually."
                .to_string(),
        ),
        "covar_pop" | "covar_samp" => FunctionTranslation::Unsupported(
            "covar_pop/covar_samp are not supported in SQLite. \
             Consider loading the statistics1 extension or computing manually."
                .to_string(),
        ),
        // Regression aggregate functions: no SQLite equivalent
        "regr_slope"
        | "regr_intercept"
        | "regr_r2"
        | "regr_avgx"
        | "regr_avgy"
        | "regr_sxx"
        | "regr_syy"
        | "regr_sxy"
        | "regr_count" => FunctionTranslation::Unsupported(
            "regr_* regression aggregate functions are not supported in SQLite. \
             Consider loading a custom extension or computing regression manually."
                .to_string(),
        ),
        // xmlagg: no XML support in SQLite
        "xmlagg" => FunctionTranslation::Unsupported(
            "xmlagg is not supported in SQLite, which has no native XML type."
                .to_string(),
        ),
        // range_agg / multirange_agg: no range type in SQLite
        "range_agg" => FunctionTranslation::Unsupported(
            "range_agg is not supported in SQLite, which has no range types."
                .to_string(),
        ),
        "multirange_agg" => FunctionTranslation::Unsupported(
            "multirange_agg is not supported in SQLite, which has no range types."
                .to_string(),
        ),
        // Ordered-set aggregates (WITHIN GROUP): handled by the WITHIN GROUP guard in translate()
        // but also listed here so they get a clear error even without WITHIN GROUP syntax.
        "percentile_cont" | "percentile_disc" => FunctionTranslation::Unsupported(
            "percentile_cont/percentile_disc are not supported in SQLite. \
             They use WITHIN GROUP (ORDER BY ...) syntax which has no SQLite equivalent."
                .to_string(),
        ),
        "mode" => FunctionTranslation::Unsupported(
            "mode() WITHIN GROUP (ORDER BY ...) is not supported in SQLite. \
             There is no built-in equivalent; consider computing the mode manually."
                .to_string(),
        ),
        // split_part has no direct SQLite equivalent
        "split_part" => FunctionTranslation::Unsupported(
            "split_part is not supported in SQLite. \
             Consider using INSTR() and SUBSTR() to manually split strings, \
             or restructure the query to avoid string splitting."
                .to_string(),
        ),
        // regexp_replace requires PCRE extension in SQLite
        "regexp_replace" => FunctionTranslation::Unsupported(
            "regexp_replace is not supported in SQLite without a PCRE extension. \
             For literal string replacement, use REPLACE(string, pattern, replacement). \
             For regex support, load the SQLite REGEXP extension."
                .to_string(),
        ),
        // to_char(expr, format) -> strftime(mapped_format, expr) for timestamp formats
        "to_char" => FunctionTranslation::ToChar,
        // json_build_object(k, v, ...) -> json_object(k, v, ...) (SQLite JSON1 built-in)
        "json_build_object" => FunctionTranslation::Rename("json_object".to_string()),
        // json_build_array(v, ...) -> json_array(v, ...) (SQLite JSON1 built-in)
        "json_build_array" | "jsonb_build_array" | "jsonb_build_object" => {
            let target = if original_name.contains("array") { "json_array" } else { "json_object" };
            FunctionTranslation::Rename(target.to_string())
        }
        // date_part('field', expr) -> CAST(strftime(format, expr) AS type)
        "date_part" => FunctionTranslation::DatePart,
        // lpad / rpad: not in standard SQLite
        "lpad" | "rpad" => FunctionTranslation::Unsupported(
            "lpad/rpad are not available in standard SQLite. \
             Consider using the printf() function or application-side string formatting."
                .to_string(),
        ),
        // initcap: not in standard SQLite
        "initcap" => FunctionTranslation::Unsupported(
            "initcap is not available in standard SQLite. \
             Consider using application-level capitalization or the ICU extension."
                .to_string(),
        ),
        // nextval: PostgreSQL sequence function, not available in SQLite
        "nextval" => FunctionTranslation::Unsupported(
            "nextval() is a PostgreSQL sequence function and is not available in SQLite. \
             Use INTEGER PRIMARY KEY (ROWID alias) or a trigger-based sequence instead."
                .to_string(),
        ),
        // generate_series: not in standard SQLite (available via an extension or recursive CTE)
        "generate_series" => {
            FunctionTranslation::Unsupported(GENERATE_SERIES_UNSUPPORTED_MESSAGE.to_string())
        }
        _ => FunctionTranslation::PassThrough,
    }
}

/// Extract expressions from function arguments.
fn extract_arg_exprs(args: &FunctionArguments) -> Vec<&Expr> {
    function_argument_exprs(args)
}

/// Convert a PostgreSQL `TO_CHAR` timestamp format string to a SQLite
/// `strftime` format.
///
/// Applies longest-first substitutions to avoid partial matches (`YYYY` before
/// `YY`, `HH24`/`HH12` before `HH`), then validates that only known strftime
/// specifiers and safe separator characters remain.
fn pg_timestamp_format_to_strftime(pg_format: &str) -> Result<String, crate::errors::Error> {
    const REPLACEMENTS: &[(&str, &str)] = &[
        ("YYYY", "%Y"),
        ("HH24", "%H"),
        ("HH12", "%I"),
        ("YY", "%y"),
        ("MM", "%m"),
        ("DD", "%d"),
        ("HH", "%I"),
        ("MI", "%M"),
        ("SS", "%S"),
    ];
    let mut result = pg_format.to_string();
    for (pg_code, strftime_code) in REPLACEMENTS {
        result = result.replace(pg_code, strftime_code);
    }
    // Validate: every % must be followed by a known specifier letter;
    // all other characters must be safe separators.
    let safe_specs: &[u8] = b"YymMdHIMS";
    let is_safe_sep = |c: char| matches!(c, '-' | ':' | '.' | '/' | ',' | '_' | ' ' | 'T');
    let mut chars = result.char_indices().peekable();
    while let Some((_i, c)) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some((_, spec)) if safe_specs.contains(&(spec as u8)) => {}
                Some((_, spec)) => {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                        "to_char format '{pg_format}' produces unsupported strftime specifier \
                         '%{spec}'. Supported PG codes: YYYY, YY, MM, DD, HH24, HH12, HH, \
                         MI, SS. For number formatting use printf() or CAST."
                    )));
                }
                None => {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                        "to_char format '{pg_format}' ends with a bare '%'"
                    )));
                }
            }
        } else if !is_safe_sep(c) {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "to_char format '{pg_format}' contains unsupported character '{c}'. \
                 Supported separators: - : . / , _ (space) T. \
                 For number formatting codes (9, 0, FM, L, …) use printf() or CAST."
            )));
        }
    }
    Ok(result)
}

/// Recursively translate all expressions inside [`FunctionArguments`].
///
/// This ensures that PostgreSQL-specific constructs nested inside function
/// arguments (e.g., `NOW()` inside `string_agg`) are properly translated.
fn translate_function_args(
    args: &FunctionArguments,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<FunctionArguments, crate::errors::Error> {
    use super::helpers::Forward;
    match args {
        FunctionArguments::None => Ok(FunctionArguments::None),
        FunctionArguments::Subquery(query) => {
            Ok(FunctionArguments::Subquery(Box::new(query.translate(schema, options)?)))
        }
        FunctionArguments::List(list) => {
            let translated = list
                .args
                .iter()
                .map(|arg| translate_function_arg(arg, schema, options))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: list.duplicate_treatment,
                args: translated,
                clauses: translate_function_argument_clauses::<Forward>(
                    &list.clauses,
                    schema,
                    options,
                )?,
            }))
        }
    }
}

/// Recursively translate a single [`FunctionArg`].
fn translate_function_arg(
    arg: &FunctionArg,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<FunctionArg, crate::errors::Error> {
    Ok(match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e.translate(schema, options)?))
        }
        FunctionArg::Named { name, arg: FunctionArgExpr::Expr(e), operator } => {
            FunctionArg::Named {
                name: name.clone(),
                arg: FunctionArgExpr::Expr(e.translate(schema, options)?),
                operator: operator.clone(),
            }
        }
        FunctionArg::ExprNamed { name, arg: FunctionArgExpr::Expr(e), operator } => {
            FunctionArg::ExprNamed {
                name: name.translate(schema, options)?,
                arg: FunctionArgExpr::Expr(e.translate(schema, options)?),
                operator: operator.clone(),
            }
        }
        other => other.clone(),
    })
}

/// Wrap an expression with COALESCE(expr, '') to handle NULL semantics.
///
/// PostgreSQL's CONCAT ignores NULL arguments; SQLite's `||` propagates them.
/// Wrapping with COALESCE ensures consistent behaviour.
fn wrap_with_coalesce(expr: Expr) -> Expr {
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("COALESCE"))]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(ValueWithSpan {
                    value: Value::SingleQuotedString(String::new()),
                    span: sqlparser::tokenizer::Span::empty(),
                }))),
            ],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
    })
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

    #[allow(clippy::too_many_lines)]
    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // Transform FILTER clause to CASE expression
        let func =
            if self.filter.is_some() { transform_filter_to_case(self) } else { self.clone() };

        // WITHIN GROUP is ordered-set aggregate syntax (percentile_cont, mode, …).
        // SQLite has no equivalent; reject early with a clear error.
        if !func.within_group.is_empty() {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "{} with WITHIN GROUP (ORDER BY …) is not supported in SQLite. \
                 Ordered-set aggregates have no SQLite equivalent.",
                func.name
            )));
        }

        match translate_function(&func.name, &func.args, options) {
            FunctionTranslation::Rename(new_name) => {
                let translated_args = translate_function_args(&func.args, schema, options)?;
                let translated_params = translate_function_args(&func.parameters, schema, options)?;
                let translated_over = translate_window_type(func.over.as_ref(), schema, options)?;
                Ok(Expr::Function(Function {
                    name: ObjectName::from(vec![Ident::new(new_name)]),
                    parameters: translated_params,
                    args: translated_args,
                    over: translated_over,
                    filter: None,
                    ..func
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
                // CONCAT(a, b, c) -> COALESCE(a, '') || COALESCE(b, '') || COALESCE(c, '')
                // PostgreSQL's CONCAT ignores NULLs; SQLite's || propagates them.
                let exprs: Vec<Expr> = extract_arg_exprs(&func.args)
                    .into_iter()
                    .map(|e| e.translate(schema, options))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .map(wrap_with_coalesce)
                    .collect();
                build_concatenation(exprs).ok_or_else(|| {
                    crate::errors::Error::UnsupportedSQLiteFeature(
                        "CONCAT requires at least one argument".to_string(),
                    )
                })
            }
            FunctionTranslation::ToConcatenationWithSeparator => {
                // CONCAT_WS(sep, a, b, c) -> COALESCE(a, '') || sep || COALESCE(b, '') || sep
                // || COALESCE(c, '') The separator is not COALESCE-wrapped: if
                // sep is NULL, the result is NULL.
                let mut exprs: Vec<Expr> = extract_arg_exprs(&func.args)
                    .into_iter()
                    .map(|e| e.translate(schema, options))
                    .collect::<Result<Vec<_>, _>>()?;
                if exprs.len() < 2 {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                        "CONCAT_WS requires at least two arguments (separator and one value)"
                            .to_string(),
                    ));
                }
                let separator = exprs.remove(0);
                let first_value = wrap_with_coalesce(exprs.remove(0));
                let remaining: Vec<Expr> = exprs.into_iter().map(wrap_with_coalesce).collect();
                Ok(build_concatenation_with_separator(&separator, first_value, remaining))
            }
            FunctionTranslation::DateTrunc => {
                // date_trunc(field, timestamp) -> strftime(format, timestamp)
                let exprs = extract_arg_exprs(&func.args);
                if exprs.len() != 2 {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                        "date_trunc requires exactly 2 arguments: date_trunc(field, timestamp)"
                            .to_string(),
                    ));
                }
                let field_expr = exprs[0];
                let ts_expr = exprs[1].clone();

                let field_str = match field_expr {
                    Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(s), .. }) => {
                        s.to_lowercase()
                    }
                    _ => {
                        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                            "date_trunc: the field argument must be a string literal \
                             (e.g., date_trunc('day', timestamp))"
                                .to_string(),
                        ));
                    }
                };

                // Map PostgreSQL truncation granularities to strftime format strings.
                // The format string zeros out the sub-granularity components.
                let format_str = match field_str.as_str() {
                    "second" | "seconds" => "%Y-%m-%d %H:%M:%S",
                    "minute" | "minutes" => "%Y-%m-%d %H:%M:00",
                    "hour" | "hours" => "%Y-%m-%d %H:00:00",
                    "day" | "days" => "%Y-%m-%d",
                    "month" | "months" => "%Y-%m-01",
                    "year" | "years" => "%Y-01-01",
                    other => {
                        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                            "date_trunc('{other}', ...) is not supported in SQLite. \
                             Supported granularities: second, minute, hour, day, month, year. \
                             Unsupported granularities (quarter, decade, century, millennium) \
                             have no strftime equivalent."
                        )));
                    }
                };

                let translated_ts = ts_expr.translate(schema, options)?;

                Ok(Expr::Function(Function {
                    name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("strftime"))]),
                    uses_odbc_syntax: false,
                    parameters: FunctionArguments::None,
                    args: FunctionArguments::List(FunctionArgumentList {
                        duplicate_treatment: None,
                        args: vec![
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                                ValueWithSpan {
                                    value: Value::SingleQuotedString(format_str.to_string()),
                                    span: sqlparser::tokenizer::Span::empty(),
                                },
                            ))),
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(translated_ts)),
                        ],
                        clauses: vec![],
                    }),
                    filter: None,
                    null_treatment: None,
                    over: None,
                    within_group: vec![],
                }))
            }
            FunctionTranslation::DatePart => {
                // date_part('field', expr) -> CAST(strftime(format, expr) AS INTEGER/REAL)
                // Semantics mirror EXTRACT(field FROM expr).
                let exprs = extract_arg_exprs(&func.args);
                if exprs.len() != 2 {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                        "date_part requires exactly 2 arguments: date_part('field', expression)"
                            .to_string(),
                    ));
                }
                let field_expr = exprs[0];
                let ts_expr = exprs[1].clone();

                let field_str = match field_expr {
                    Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(s), .. }) => {
                        s.to_lowercase()
                    }
                    _ => {
                        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                            "date_part: the field argument must be a string literal \
                             (e.g., date_part('year', timestamp))"
                                .to_string(),
                        ));
                    }
                };

                let (format_str, cast_type) = match field_str.as_str() {
                    "year" | "years" => ("%Y", DataType::Integer(None)),
                    "month" | "months" => ("%m", DataType::Integer(None)),
                    "day" | "days" => ("%d", DataType::Integer(None)),
                    "hour" | "hours" => ("%H", DataType::Integer(None)),
                    "minute" | "minutes" => ("%M", DataType::Integer(None)),
                    "second" | "seconds" => ("%f", DataType::Real),
                    "week" | "weeks" => ("%W", DataType::Integer(None)),
                    "dow" | "weekday" => ("%w", DataType::Integer(None)),
                    "doy" => ("%j", DataType::Integer(None)),
                    "epoch" => ("%s", DataType::Real),
                    other => {
                        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                            "date_part('{other}', ...) is not supported in SQLite. \
                             Supported fields: year, month, day, hour, minute, second, \
                             week, dow, doy, epoch."
                        )));
                    }
                };

                let translated_ts = ts_expr.translate(schema, options)?;

                let strftime_call = Expr::Function(Function {
                    name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("strftime"))]),
                    uses_odbc_syntax: false,
                    parameters: FunctionArguments::None,
                    args: FunctionArguments::List(FunctionArgumentList {
                        duplicate_treatment: None,
                        args: vec![
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                                ValueWithSpan {
                                    value: Value::SingleQuotedString(format_str.to_string()),
                                    span: sqlparser::tokenizer::Span::empty(),
                                },
                            ))),
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(translated_ts)),
                        ],
                        clauses: vec![],
                    }),
                    filter: None,
                    null_treatment: None,
                    over: func
                        .over
                        .as_ref()
                        .map(|w| translate_window_type(Some(w), schema, options))
                        .transpose()?
                        .flatten(),
                    within_group: vec![],
                });

                Ok(Expr::Cast {
                    expr: Box::new(strftime_call),
                    data_type: cast_type,
                    format: None,
                    kind: CastKind::Cast,
                    array: false,
                })
            }
            FunctionTranslation::ToChar => {
                // to_char(expr, format) -> strftime(mapped_format, expr)
                // Note: PG arg order is (expr, format); SQLite strftime arg order is (format,
                // expr).
                let exprs = extract_arg_exprs(&func.args);
                if exprs.len() != 2 {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                        "to_char requires exactly 2 arguments: to_char(expression, format)"
                            .to_string(),
                    ));
                }
                let ts_expr = exprs[0].clone();
                let format_expr = exprs[1];
                let format_str = match format_expr {
                    Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(s), .. }) => {
                        s.clone()
                    }
                    _ => {
                        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                            "to_char: format argument must be a string literal known at \
                             translation time (e.g., to_char(col, 'YYYY-MM-DD')). Dynamic \
                             formats cannot be translated."
                                .to_string(),
                        ));
                    }
                };
                let mapped_format = pg_timestamp_format_to_strftime(&format_str)?;
                let translated_ts = ts_expr.translate(schema, options)?;
                Ok(Expr::Function(Function {
                    name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("strftime"))]),
                    uses_odbc_syntax: false,
                    parameters: FunctionArguments::None,
                    args: FunctionArguments::List(FunctionArgumentList {
                        duplicate_treatment: None,
                        args: vec![
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                                ValueWithSpan {
                                    value: Value::SingleQuotedString(mapped_format),
                                    span: sqlparser::tokenizer::Span::empty(),
                                },
                            ))),
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(translated_ts)),
                        ],
                        clauses: vec![],
                    }),
                    filter: None,
                    null_treatment: None,
                    over: None,
                    within_group: vec![],
                }))
            }
            FunctionTranslation::Unsupported(msg) => {
                Err(crate::errors::Error::UnsupportedSQLiteFeature(msg))
            }
            FunctionTranslation::PassThrough => {
                let translated_args = translate_function_args(&func.args, schema, options)?;
                let translated_params = translate_function_args(&func.parameters, schema, options)?;
                let translated_over = translate_window_type(func.over.as_ref(), schema, options)?;
                Ok(Expr::Function(Function {
                    parameters: translated_params,
                    args: translated_args,
                    over: translated_over,
                    ..func
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
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
        wrap_arg_with_case_filter, wrap_with_coalesce,
    };
    use crate::prelude::{Pg2SqliteOptions, Translator};

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

    #[test]
    fn concat_ws_supports_expr_named_arguments() {
        let schema =
            ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build");
        let options = Pg2SqliteOptions::default();
        let func = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("concat_ws"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![
                    FunctionArg::ExprNamed {
                        name: parse_expr("sep"),
                        arg: FunctionArgExpr::Expr(parse_expr("','")),
                        operator: FunctionArgOperator::Equals,
                    },
                    FunctionArg::ExprNamed {
                        name: parse_expr("lhs"),
                        arg: FunctionArgExpr::Expr(parse_expr("first_name")),
                        operator: FunctionArgOperator::Equals,
                    },
                    FunctionArg::ExprNamed {
                        name: parse_expr("rhs"),
                        arg: FunctionArgExpr::Expr(parse_expr("last_name")),
                        operator: FunctionArgOperator::Equals,
                    },
                ],
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
        };

        let translated = func.translate(&schema, &options).expect("concat_ws should translate");
        // With COALESCE wrapping: COALESCE(first_name, '') || ',' ||
        // COALESCE(last_name, '')
        assert!(
            translated.to_string().contains("COALESCE"),
            "concat_ws should wrap values with COALESCE: {}",
            translated
        );
        assert!(
            translated.to_string().contains("first_name"),
            "concat_ws should preserve column names: {}",
            translated
        );
    }

    #[test]
    fn wrap_with_coalesce_wraps_expr_with_empty_string_default() {
        let expr = parse_expr("col");
        let wrapped = wrap_with_coalesce(expr);
        assert_eq!(wrapped.to_string(), "COALESCE(col, '')");
    }
}
