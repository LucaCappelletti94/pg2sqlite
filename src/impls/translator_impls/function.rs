//! Implementation of the [`Translator`] trait for the
//! `Function` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, CastKind, DataType, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArgumentList, FunctionArguments, Ident, ObjectName, Value, ValueWithSpan,
};

use super::{
    helpers::{Forward, translate_window_type},
    postgis,
};
use crate::{
    impls::{
        datetime_helpers::{build_strftime_call, parse_date_part_key, strftime_mapping_for_key},
        function_helpers::{
            extract_exactly, number_literal, simple_function_expr, string_literal, unnamed_arg,
        },
        shared_helpers::{
            GENERATE_SERIES_UNSUPPORTED_MESSAGE, function_argument_exprs,
            translate_function_arguments,
        },
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
    /// Transform random() to ABS(random()) / 9223372036854775807.0
    ToRandomFloat,
    /// Transform left(s, n) to substr(s, 1, n)
    ToSubstrLeft,
    /// Transform right(s, n) to substr(s, -n)
    ToSubstrRight,
    /// Transform to_timestamp(epoch) to datetime(epoch, 'unixepoch')
    ToTimestampEpoch,
    /// Transform mod(a, b) to (a % b)
    ToModulo,
    /// Transform div(a, b) to CAST(a / b AS INTEGER)
    ToIntegerDiv,
    /// Transform trunc(x) to CAST(x AS INTEGER), trunc(x, n) to round(x, n)
    ToTrunc,
    /// Transform make_date/make_time/make_timestamp to printf(format, args...)
    ToMakePrintf { format: &'static str, arg_count: usize, func_label: &'static str },
    /// Transform json_extract_path(j, keys...) to json_extract(j, '$.k1.k2...')
    ToJsonExtractPath,
    /// No translation needed
    PassThrough,
}

/// Simple name-only renames: `(pg_name, sqlite_name)`.
/// Checked before the main match for a compact fast path.
const FORWARD_RENAMES: &[(&str, &str)] = &[
    ("least", "MIN"),
    ("greatest", "MAX"),
    ("string_agg", "group_concat"),
    ("strpos", "INSTR"),
    ("chr", "char"),
    ("char_length", "length"),
    ("character_length", "length"),
    ("json_agg", "json_group_array"),
    ("jsonb_agg", "json_group_array"),
    ("json_object_agg", "json_group_object"),
    ("jsonb_object_agg", "json_group_object"),
    ("json_build_object", "json_object"),
    ("btrim", "trim"),
    ("jsonb_array_length", "json_array_length"),
    ("json_typeof", "json_type"),
    ("jsonb_typeof", "json_type"),
    ("quote_nullable", "quote"),
    ("quote_literal", "quote"),
    ("version", "sqlite_version"),
    ("to_json", "json"),
    ("to_jsonb", "json"),
    ("jsonb_set", "json_set"),
    ("jsonb_insert", "json_insert"),
    ("jsonb_each", "json_each"),
    ("json_each_text", "json_each"),
    ("jsonb_each_text", "json_each"),
    ("ascii", "unicode"),
];

#[allow(clippy::too_many_lines)]
fn translate_function(
    name: &ObjectName,
    args: &FunctionArguments,
    options: &Pg2SqliteOptions,
) -> FunctionTranslation {
    let original_name = name
        .0
        .last()
        .and_then(|part| part.as_ident())
        .map_or_else(|| name.to_string().to_lowercase(), |ident| ident.value.to_ascii_lowercase());

    // Fast path: check static rename table first.
    if let Some(&(_, target)) =
        FORWARD_RENAMES.iter().find(|&&(pg, _)| pg == original_name.as_str())
    {
        return FunctionTranslation::Rename(target.to_string());
    }

    match original_name.as_str() {
        // bool_and / bool_or / every
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
        // NOW() -> datetime('now')
        "now" => FunctionTranslation::WithArgs {
            name: "datetime".to_string(),
            args: vec![unnamed_arg(string_literal("now"))],
        },
        "ts_rank" | "ts_rank_cd" => FunctionTranslation::Unsupported(
            "ts_rank/ts_rank_cd are not directly translatable to SQLite. \
             FTS5 provides bm25() for ranking, but it requires a different query structure. \
             Consider querying the FTS5 table directly: \
             SELECT *, bm25(table_fts) AS rank FROM table_fts WHERE table_fts MATCH 'query' ORDER BY rank"
                .to_string(),
        ),
        "concat" => FunctionTranslation::ToConcatenation,
        "concat_ws" => FunctionTranslation::ToConcatenationWithSeparator,
        "date_trunc" => FunctionTranslation::DateTrunc,
        "array_agg" => FunctionTranslation::Unsupported(
            "array_agg is not supported in SQLite because arrays are not a native type. \
             Use group_concat() instead: GROUP_CONCAT(column, ',')"
                .to_string(),
        ),
        "bit_and" | "bit_or" => FunctionTranslation::Unsupported(format!(
            "{original_name} is not supported as an aggregate in SQLite. \
             Consider loading a custom extension or rewriting with bitwise expressions."
        )),
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
        "regr_slope" | "regr_intercept" | "regr_r2" | "regr_avgx" | "regr_avgy"
        | "regr_sxx" | "regr_syy" | "regr_sxy" | "regr_count" => {
            FunctionTranslation::Unsupported(
                "regr_* regression aggregate functions are not supported in SQLite. \
                 Consider loading a custom extension or computing regression manually."
                    .to_string(),
            )
        }
        "xmlagg" => FunctionTranslation::Unsupported(
            "xmlagg is not supported in SQLite, which has no native XML type.".to_string(),
        ),
        "range_agg" | "multirange_agg" => FunctionTranslation::Unsupported(format!(
            "{original_name} is not supported in SQLite, which has no range types."
        )),
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
        "split_part" => FunctionTranslation::Unsupported(
            "split_part is not supported in SQLite. \
             Consider using INSTR() and SUBSTR() to manually split strings, \
             or restructure the query to avoid string splitting."
                .to_string(),
        ),
        "regexp_replace" => FunctionTranslation::Unsupported(
            "regexp_replace is not supported in SQLite without a PCRE extension. \
             For literal string replacement, use REPLACE(string, pattern, replacement). \
             For regex support, load the SQLite REGEXP extension."
                .to_string(),
        ),
        "to_char" => FunctionTranslation::ToChar,
        // json_build_array(v, ...) -> json_array(v, ...) (handle remaining jsonb_build_*)
        "jsonb_build_array" => FunctionTranslation::Rename("json_array".to_string()),
        "jsonb_build_object" => FunctionTranslation::Rename("json_object".to_string()),
        // localtimestamp -> datetime('now', 'localtime')
        "localtimestamp" => FunctionTranslation::WithArgs {
            name: "datetime".to_string(),
            args: vec![unnamed_arg(string_literal("now")), unnamed_arg(string_literal("localtime"))],
        },
        // localtime -> time('now', 'localtime')
        "localtime" => FunctionTranslation::WithArgs {
            name: "time".to_string(),
            args: vec![unnamed_arg(string_literal("now")), unnamed_arg(string_literal("localtime"))],
        },
        "mod" => FunctionTranslation::ToModulo,
        "div" => FunctionTranslation::ToIntegerDiv,
        "trunc" | "truncate" => FunctionTranslation::ToTrunc,
        "make_date" => FunctionTranslation::ToMakePrintf {
            format: "%04d-%02d-%02d",
            arg_count: 3,
            func_label: "make_date",
        },
        "make_time" => FunctionTranslation::ToMakePrintf {
            format: "%02d:%02d:%02d",
            arg_count: 3,
            func_label: "make_time",
        },
        "make_timestamp" => FunctionTranslation::ToMakePrintf {
            format: "%04d-%02d-%02d %02d:%02d:%02d",
            arg_count: 6,
            func_label: "make_timestamp",
        },
        // json_extract_path* -> json_extract(j, '$.k1.k2...')
        "json_extract_path" | "json_extract_path_text" | "jsonb_extract_path"
        | "jsonb_extract_path_text" => FunctionTranslation::ToJsonExtractPath,
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
        // random(): PG returns [0.0, 1.0) float; SQLite returns signed 64-bit int.
        // Map to (CAST(random() AS REAL) + 2^63) / 2^64 → [0.0, 1.0) without ABS overflow.
        "random" => FunctionTranslation::ToRandomFloat,
        // left(s, n) -> substr(s, 1, n)
        "left" => FunctionTranslation::ToSubstrLeft,
        // right(s, n) -> substr(s, -n)
        "right" => FunctionTranslation::ToSubstrRight,
        // to_timestamp(epoch) -> datetime(val, 'unixepoch') (single-arg form)
        // to_timestamp(text, format) -> Unsupported (two-arg form)
        "to_timestamp" => {
            match args {
                FunctionArguments::List(list) if list.args.len() == 1 => {
                    FunctionTranslation::ToTimestampEpoch
                }
                _ => FunctionTranslation::Unsupported(
                    "to_timestamp with format string is not supported in SQLite. \
                     Only the single-argument epoch form (to_timestamp(epoch_seconds)) \
                     can be translated."
                        .to_string(),
                ),
            }
        }
        // transaction_timestamp / statement_timestamp / clock_timestamp → datetime('now')
        "transaction_timestamp" | "statement_timestamp" | "clock_timestamp" => {
            FunctionTranslation::WithArgs {
                name: "datetime".to_string(),
                args: vec![unnamed_arg(string_literal("now"))],
            }
        }
        // Sequence functions: no SQLite equivalent
        "currval" | "lastval" | "setval" => FunctionTranslation::Unsupported(
            "currval/lastval/setval are PostgreSQL sequence functions and are not available \
             in SQLite. Use INTEGER PRIMARY KEY (ROWID alias) or application-level sequences."
                .to_string(),
        ),
        // repeat: no simple SQLite equivalent
        "repeat" => FunctionTranslation::Unsupported(
            "repeat is not available in standard SQLite. \
             Consider using application-level string repetition or a recursive CTE."
                .to_string(),
        ),
        // translate: character-level replacement, no SQLite equivalent
        "translate" => FunctionTranslation::Unsupported(
            "translate (character-level replacement) is not available in SQLite. \
             Consider using nested REPLACE() calls or application-level processing."
                .to_string(),
        ),
        // md5: no hash function in core SQLite
        "md5" => FunctionTranslation::Unsupported(
            "md5 is not available in standard SQLite. \
             Consider loading a custom extension for hashing."
                .to_string(),
        ),
        // to_date: format-based date parsing not available in SQLite
        "to_date" => FunctionTranslation::Unsupported(
            "to_date is not supported in SQLite. \
             Date strings must be in ISO 8601 format (YYYY-MM-DD) for SQLite date functions."
                .to_string(),
        ),
        // age: returns interval type, no SQLite equivalent
        "age" => FunctionTranslation::Unsupported(
            "age is not supported in SQLite, which has no interval type. \
             Consider using julianday() subtraction for day-level differences."
                .to_string(),
        ),
        // regexp_match / regexp_matches: no built-in regex in SQLite
        "regexp_match" | "regexp_matches" => FunctionTranslation::Unsupported(
            "regexp_match/regexp_matches are not supported in SQLite without a REGEXP extension. \
             For basic pattern matching use LIKE or GLOB."
                .to_string(),
        ),
        // array_length / array_to_string: no array type in SQLite
        "array_length" | "array_to_string" => FunctionTranslation::Unsupported(
            "array_length/array_to_string are not supported in SQLite, which has no native \
             array type. Consider using JSON arrays with json_array_length() or \
             json_group_array()."
                .to_string(),
        ),
        // format: PG format specifiers incompatible with SQLite printf
        "format" => FunctionTranslation::Unsupported(
            "format() with PostgreSQL format specifiers (%I, %L, %s) is not supported in SQLite. \
             For simple string formatting use printf() with standard C-style specifiers."
                .to_string(),
        ),
        // PG-specific system/introspection functions - no SQLite equivalent
        "current_database" | "current_schema" | "pg_typeof" => FunctionTranslation::Unsupported(
            "current_database/current_schema/pg_typeof are PostgreSQL system functions \
             with no SQLite equivalent."
                .to_string(),
        ),
        // unnest: requires array type
        "unnest" => FunctionTranslation::Unsupported(
            "unnest() requires PostgreSQL's array type, which has no SQLite equivalent. \
             Consider using json_each() for JSON arrays."
                .to_string(),
        ),
        // encode/decode: PG bytea encoding functions
        "encode" | "decode" => FunctionTranslation::Unsupported(
            "encode/decode are PostgreSQL bytea encoding functions with no direct \
             SQLite equivalent. Consider using hex()/unhex() for hexadecimal conversion."
                .to_string(),
        ),
        // to_number: PG pattern-based number parsing
        "to_number" => FunctionTranslation::Unsupported(
            "to_number() with PostgreSQL format patterns is not supported in SQLite. \
             Use CAST(expr AS REAL) or CAST(expr AS INTEGER) for simple conversions."
                .to_string(),
        ),
        // --- GROUP P: Bulk unsupported functions ---

        // String functions with no SQLite equivalent
        "regexp_split_to_array" | "regexp_split_to_table" | "string_to_array" => {
            FunctionTranslation::Unsupported(format!(
                "{original_name}() is not available in standard SQLite."
            ))
        }
        "quote_ident" => FunctionTranslation::Unsupported(
            "quote_ident() is not available in SQLite. Use application-level quoting.".to_string(),
        ),
        "convert" | "convert_from" | "convert_to" => FunctionTranslation::Unsupported(format!(
            "{original_name}() character encoding conversion is not available in SQLite."
        )),

        // Math functions requiring extensions
        "log" | "ln" | "exp" | "sqrt" | "cbrt" | "power" => {
            FunctionTranslation::Unsupported(format!(
                "{original_name}() is not available in standard SQLite. \
                 Consider using the math extension or application-level computation."
            ))
        }
        "sign" | "factorial" | "gcd" | "lcm" | "pi" | "degrees" | "radians" | "setseed"
        | "width_bucket" => FunctionTranslation::Unsupported(format!(
            "{original_name}() is not available in standard SQLite."
        )),

        // Date/time and JSON functions with no equivalent
        "make_timestamptz" | "make_interval" | "isfinite" | "json_strip_nulls"
        | "jsonb_strip_nulls" => FunctionTranslation::Unsupported(format!(
            "{original_name}() is not available in SQLite."
        )),
        "justify_days" | "justify_hours" | "justify_interval" => {
            FunctionTranslation::Unsupported(format!(
                "{original_name}() is not available in SQLite (no interval type)."
            ))
        }
        "timeofday" => FunctionTranslation::Unsupported(
            "timeofday() is not available in SQLite. Use datetime('now') instead.".to_string(),
        ),
        "json_populate_record" | "jsonb_populate_record" => {
            FunctionTranslation::Unsupported(format!(
                "{original_name}() is not available in SQLite (no record types)."
            ))
        }
        "json_to_record" | "jsonb_to_record" | "row_to_json" => {
            FunctionTranslation::Unsupported(format!(
                "{original_name}() is not available in SQLite (no record/row types)."
            ))
        }

        // Array functions (no native array type)
        "array_append" | "array_cat" | "array_dims" | "array_fill" | "array_lower"
        | "array_upper" | "array_ndims" | "array_position" | "array_positions"
        | "array_prepend" | "array_remove" | "array_replace" | "cardinality" => {
            FunctionTranslation::Unsupported(format!(
                "{original_name}() is not available in SQLite (no native array type). \
                 Consider using JSON arrays with json_group_array() and json_each()."
            ))
        }

        // Network functions
        "host" | "abbrev" | "broadcast" | "family" | "hostmask" | "masklen" | "netmask"
        | "network" | "set_masklen" => FunctionTranslation::Unsupported(format!(
            "{original_name}() is not available in SQLite (no network address types)."
        )),

        // System catalog functions
        "current_schemas" | "has_table_privilege" | "has_schema_privilege"
        | "has_column_privilege" | "has_database_privilege" | "has_function_privilege"
        | "has_sequence_privilege" => FunctionTranslation::Unsupported(format!(
            "{original_name}() is a PostgreSQL catalog function not available in SQLite."
        )),
        "obj_description" | "col_description" | "shobj_description" | "pg_get_expr"
        | "pg_get_constraintdef" | "pg_get_indexdef" | "pg_get_viewdef" => {
            FunctionTranslation::Unsupported(format!(
                "{original_name}() is a PostgreSQL catalog function not available in SQLite."
            ))
        }
        "pg_table_size" | "pg_total_relation_size" | "pg_relation_size" | "pg_column_size"
        | "pg_database_size" | "pg_tablespace_size" => FunctionTranslation::Unsupported(format!(
            "{original_name}() is a PostgreSQL size function not available in SQLite."
        )),

        _ => maybe_geolite_passthrough(&original_name, args, options),
    }
}

/// When `enable_geolite` is on, validate `ST_*`-shaped calls against the
/// catalog mirrored from geolite (`super::postgis`). Functions in the
/// catalog with matching arity pass through. Arity mismatches and unknown
/// `st_*` names error with a precise message. With `enable_geolite` off
/// this is a no-op and unknown functions keep their pre-existing
/// passthrough behavior.
fn maybe_geolite_passthrough(
    name: &str,
    args: &FunctionArguments,
    options: &Pg2SqliteOptions,
) -> FunctionTranslation {
    if !options.is_geolite_enabled() {
        return FunctionTranslation::PassThrough;
    }
    let Some(arity) = function_arg_count(args) else {
        return FunctionTranslation::PassThrough;
    };
    if postgis::is_geolite_function(name, arity) {
        return FunctionTranslation::PassThrough;
    }
    let known_arities = postgis::geolite_function_arities(name);
    if !known_arities.is_empty() {
        return FunctionTranslation::Unsupported(format!(
            "{name}/{arity} is not in the geolite catalog; geolite implements arities {known_arities:?} for this name."
        ));
    }
    if postgis::is_postgis_shaped_name(name) {
        return FunctionTranslation::Unsupported(format!(
            "{name}() looks like a PostGIS function but is not implemented by the geolite SQLite extension; \
             see https://github.com/LucaCappelletti94/geolite for the supported list."
        ));
    }
    FunctionTranslation::PassThrough
}

/// Returns the positional arg count when it can be determined from the
/// `FunctionArguments` shape, or `None` for subquery-shaped arguments
/// where positional arity isn't meaningful.
fn function_arg_count(args: &FunctionArguments) -> Option<i32> {
    match args {
        FunctionArguments::List(list) => i32::try_from(list.args.len()).ok(),
        FunctionArguments::None => Some(0),
        FunctionArguments::Subquery(_) => None,
    }
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
                 For number formatting codes (9, 0, FM, L, ...) use printf() or CAST."
            )));
        }
    }
    Ok(result)
}

/// Wrap an expression with COALESCE(expr, '') to handle NULL semantics.
///
/// PostgreSQL's CONCAT ignores NULL arguments. SQLite's `||` propagates them.
/// Wrapping with COALESCE ensures consistent behaviour.
fn wrap_with_coalesce(expr: Expr) -> Expr {
    simple_function_expr("COALESCE", vec![expr, string_literal("")], None)
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
fn build_case_when(condition: Expr, then_expr: Expr, else_expr: Expr) -> Expr {
    Expr::Case {
        case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
        end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
        operand: None,
        conditions: vec![sqlparser::ast::CaseWhen { condition, result: then_expr }],
        else_result: Some(Box::new(else_expr)),
    }
}

fn any_not_null_condition(exprs: &[Expr]) -> Option<Expr> {
    let mut iter = exprs.iter();
    let first = iter.next()?.clone();
    let mut condition = Expr::IsNotNull(Box::new(first));
    for expr in iter {
        condition = Expr::BinaryOp {
            left: Box::new(condition),
            op: BinaryOperator::Or,
            right: Box::new(Expr::IsNotNull(Box::new(expr.clone()))),
        };
    }
    Some(condition)
}

fn build_concat_ws_piece(value: Expr, separator: &Expr, prior_values: &[Expr]) -> Expr {
    let prefixed_value = if let Some(has_prior_non_null) = any_not_null_condition(prior_values) {
        let prefix = build_case_when(has_prior_non_null, separator.clone(), string_literal(""));
        Expr::BinaryOp {
            left: Box::new(prefix),
            op: BinaryOperator::StringConcat,
            right: Box::new(value.clone()),
        }
    } else {
        value.clone()
    };

    // PostgreSQL CONCAT_WS skips NULL values entirely.
    build_case_when(Expr::IsNull(Box::new(value)), string_literal(""), prefixed_value)
}

fn build_concat_ws_expression(separator: &Expr, values: Vec<Expr>) -> Option<Expr> {
    if values.is_empty() {
        return None;
    }

    let mut pieces = Vec::with_capacity(values.len());
    let mut prior_values = Vec::with_capacity(values.len());

    for value in values {
        let piece = build_concat_ws_piece(value.clone(), separator, &prior_values);
        pieces.push(piece);
        prior_values.push(value);
    }

    build_concatenation(pieces)
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
        FunctionArg::ExprNamed { name, arg: FunctionArgExpr::Expr(expr), operator } => {
            FunctionArg::ExprNamed {
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

        // WITHIN GROUP is ordered-set aggregate syntax (percentile_cont, mode, ...).
        // SQLite has no equivalent; reject early with a clear error.
        if !func.within_group.is_empty() {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "{} with WITHIN GROUP (ORDER BY ...) is not supported in SQLite. \
                 Ordered-set aggregates have no SQLite equivalent.",
                func.name
            )));
        }

        match translate_function(&func.name, &func.args, options) {
            FunctionTranslation::Rename(new_name) => {
                let translated_args =
                    translate_function_arguments::<Forward>(&func.args, schema, options)?;
                let translated_params =
                    translate_function_arguments::<Forward>(&func.parameters, schema, options)?;
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
                let translated_over = translate_window_type(func.over.as_ref(), schema, options)?;
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
                    over: translated_over,
                    within_group: vec![],
                }))
            }
            FunctionTranslation::ToConcatenation => {
                // CONCAT(a, b, c) -> COALESCE(a, '') || COALESCE(b, '') || COALESCE(c, '')
                // PostgreSQL's CONCAT ignores NULLs; SQLite's || propagates them.
                let exprs: Vec<Expr> = function_argument_exprs(&func.args)
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
                // CONCAT_WS(sep, a, b, c) skips NULL value args and only inserts the
                // separator between non-NULL values.
                let mut exprs: Vec<Expr> = function_argument_exprs(&func.args)
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
                build_concat_ws_expression(&separator, exprs).ok_or_else(|| {
                    crate::errors::Error::UnsupportedSQLiteFeature(
                        "CONCAT_WS requires at least one value argument".to_string(),
                    )
                })
            }
            FunctionTranslation::DateTrunc => {
                // date_trunc(field, timestamp) -> strftime(format, timestamp)
                let exprs = extract_exactly(&func.args, 2, "date_trunc")?;
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
                let translated_over = translate_window_type(func.over.as_ref(), schema, options)?;
                Ok(build_strftime_call(format_str, translated_ts, translated_over))
            }
            FunctionTranslation::DatePart => {
                // date_part('field', expr) -> CAST(strftime(format, expr) AS INTEGER/REAL)
                let exprs = extract_exactly(&func.args, 2, "date_part")?;
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

                let key = parse_date_part_key(&field_str).ok_or_else(|| {
                    crate::errors::Error::UnsupportedSQLiteFeature(format!(
                        "date_part('{field_str}', ...) is not supported in SQLite. \
                         Supported fields: year, month, day, hour, minute, second, \
                         week, dow, doy, epoch."
                    ))
                })?;
                let (format_str, cast_type) = strftime_mapping_for_key(key);

                let translated_ts = ts_expr.translate(schema, options)?;
                let translated_over = translate_window_type(func.over.as_ref(), schema, options)?;
                let strftime_call = build_strftime_call(format_str, translated_ts, translated_over);

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
                let exprs = extract_exactly(&func.args, 2, "to_char")?;
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
                let translated_over = translate_window_type(func.over.as_ref(), schema, options)?;
                Ok(build_strftime_call(&mapped_format, translated_ts, translated_over))
            }
            FunctionTranslation::ToRandomFloat => {
                // random() -> (CAST(random() AS REAL) + 9223372036854775808.0) /
                // 18446744073709551616.0 This avoids ABS(-9223372036854775808)
                // overflow in SQLite.
                let random_call = simple_function_expr("random", vec![], None);
                let random_as_real = Expr::Cast {
                    expr: Box::new(random_call),
                    data_type: DataType::Real,
                    format: None,
                    kind: CastKind::Cast,
                    array: false,
                };
                let shifted = Expr::BinaryOp {
                    left: Box::new(random_as_real),
                    op: BinaryOperator::Plus,
                    right: Box::new(number_literal("9223372036854775808.0")),
                };
                Ok(Expr::BinaryOp {
                    left: Box::new(Expr::Nested(Box::new(shifted))),
                    op: BinaryOperator::Divide,
                    right: Box::new(number_literal("18446744073709551616.0")),
                })
            }
            FunctionTranslation::ToSubstrLeft => {
                // left(s, n) → substr(s, 1, n)
                let exprs = extract_exactly(&func.args, 2, "left")?;
                let s = exprs[0].translate(schema, options)?;
                let n = exprs[1].translate(schema, options)?;
                Ok(simple_function_expr("substr", vec![s, number_literal("1"), n], None))
            }
            FunctionTranslation::ToSubstrRight => {
                // right(s, n) → substr(s, -n)
                let exprs = extract_exactly(&func.args, 2, "right")?;
                let s = exprs[0].translate(schema, options)?;
                let n = exprs[1].translate(schema, options)?;
                let neg_n =
                    Expr::UnaryOp { op: sqlparser::ast::UnaryOperator::Minus, expr: Box::new(n) };
                Ok(simple_function_expr("substr", vec![s, neg_n], None))
            }
            FunctionTranslation::ToTimestampEpoch => {
                // to_timestamp(epoch) → datetime(epoch, 'unixepoch')
                let exprs = extract_exactly(&func.args, 1, "to_timestamp")?;
                let epoch = exprs[0].translate(schema, options)?;
                Ok(simple_function_expr("datetime", vec![epoch, string_literal("unixepoch")], None))
            }
            FunctionTranslation::ToModulo => {
                // mod(a, b) → (a % b)
                let exprs = extract_exactly(&func.args, 2, "mod")?;
                let left = exprs[0].translate(schema, options)?;
                let right = exprs[1].translate(schema, options)?;
                Ok(Expr::Nested(Box::new(Expr::BinaryOp {
                    left: Box::new(left),
                    op: BinaryOperator::Modulo,
                    right: Box::new(right),
                })))
            }
            FunctionTranslation::ToIntegerDiv => {
                // div(a, b) → CAST(a / b AS INTEGER)
                let exprs = extract_exactly(&func.args, 2, "div")?;
                let left = exprs[0].translate(schema, options)?;
                let right = exprs[1].translate(schema, options)?;
                Ok(Expr::Cast {
                    expr: Box::new(Expr::BinaryOp {
                        left: Box::new(left),
                        op: BinaryOperator::Divide,
                        right: Box::new(right),
                    }),
                    data_type: DataType::Integer(None),
                    format: None,
                    kind: CastKind::Cast,
                    array: false,
                })
            }
            FunctionTranslation::ToTrunc => {
                let exprs = function_argument_exprs(&func.args);
                match exprs.len() {
                    // trunc(x) → CAST(x AS INTEGER)
                    1 => {
                        let expr = exprs[0].translate(schema, options)?;
                        Ok(Expr::Cast {
                            expr: Box::new(expr),
                            data_type: DataType::Integer(None),
                            format: None,
                            kind: CastKind::Cast,
                            array: false,
                        })
                    }
                    // trunc(x, n) → round(x, n) (approximate, lossy)
                    2 => {
                        let x = exprs[0].translate(schema, options)?;
                        let n = exprs[1].translate(schema, options)?;
                        Ok(simple_function_expr("round", vec![x, n], None))
                    }
                    _ => {
                        Err(crate::errors::Error::UnsupportedSQLiteFeature(
                            "trunc() requires 1 or 2 arguments".to_string(),
                        ))
                    }
                }
            }
            FunctionTranslation::ToMakePrintf { format, arg_count, func_label } => {
                let exprs = extract_exactly(&func.args, arg_count, func_label)?;
                let mut printf_args = vec![string_literal(format)];
                for e in &exprs {
                    printf_args.push(e.translate(schema, options)?);
                }
                Ok(simple_function_expr("printf", printf_args, None))
            }
            FunctionTranslation::ToJsonExtractPath => {
                // json_extract_path(j, 'k1', 'k2') → json_extract(j, '$.k1.k2')
                let exprs = function_argument_exprs(&func.args);
                if exprs.len() < 2 {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                        "json_extract_path requires at least 2 arguments".to_string(),
                    ));
                }
                let json_expr = exprs[0].translate(schema, options)?;
                let mut path = String::from("$");
                for key_expr in &exprs[1..] {
                    if let Expr::Value(ValueWithSpan {
                        value: Value::SingleQuotedString(key),
                        ..
                    }) = key_expr
                    {
                        path.push('.');
                        path.push_str(key);
                    } else {
                        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                            "json_extract_path requires string literal keys for SQLite translation"
                                .to_string(),
                        ));
                    }
                }
                Ok(simple_function_expr(
                    "json_extract",
                    vec![json_expr, string_literal(&path)],
                    None,
                ))
            }
            FunctionTranslation::Unsupported(msg) => {
                Err(crate::errors::Error::UnsupportedSQLiteFeature(msg))
            }
            FunctionTranslation::PassThrough => {
                let translated_args =
                    translate_function_arguments::<Forward>(&func.args, schema, options)?;
                let translated_params =
                    translate_function_arguments::<Forward>(&func.parameters, schema, options)?;
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
        build_concat_ws_expression, transform_filter_to_case, wrap_arg_with_case_filter,
        wrap_with_coalesce,
    };
    use crate::{
        impls::shared_helpers::function_argument_exprs,
        prelude::{Pg2SqliteOptions, Translator},
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
        assert!(function_argument_exprs(&FunctionArguments::None).is_empty());

        let concatenated = build_concat_ws_expression(
            &parse_expr("','"),
            vec![parse_expr("a"), parse_expr("b"), parse_expr("c")],
        )
        .expect("concat_ws helper should return expression");
        let sql = concatenated.to_string();
        assert!(sql.contains("CASE WHEN"), "expected CASE-based concat_ws expression: {sql}");
        assert!(
            sql.contains("||"),
            "expected concatenation operators in concat_ws expression: {sql}"
        );

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
        assert!(
            translated.to_string().contains("CASE WHEN"),
            "concat_ws should use CASE expressions to skip NULL values: {}",
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
