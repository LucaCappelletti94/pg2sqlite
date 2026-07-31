//! Implementation of the [`Translator`] trait for the
//! `Function` type.

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

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, CaseWhen, CastKind, DataType, DuplicateTreatment, Expr, Function, FunctionArg,
    FunctionArgExpr, FunctionArgumentList, FunctionArguments, Ident, ObjectName, ObjectNamePart,
    UnaryOperator, Value, ValueWithSpan, helpers::attached_token::AttachedToken,
};

use super::{
    array::{self, ArrayFunction},
    helpers::{Forward, translate_window_type},
    postgis,
};
use crate::{
    impls::{
        datetime_helpers::{build_strftime_call, parse_date_part_key, strftime_mapping_for_key},
        expr_helpers::case_when,
        function_helpers::{
            extract_exactly, integer_literal, number_literal, simple_function_expr, string_literal,
            unnamed_arg,
        },
        shared_helpers::{
            GENERATE_SERIES_UNSUPPORTED_MESSAGE, every_declared_type_matches,
            function_argument_exprs, numeric_scale, rescale_minor_units,
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
    /// Transform `jsonb_set` and `jsonb_insert` to the `json_set`,
    /// `json_replace`, or `json_insert` that matches, converting the `text[]`
    /// path to JSONPath and keeping the value typed as JSON.
    JsonSet { insert: bool },
    /// Transform `to_json` and `to_jsonb` to `json_quote`, which CONVERTS a
    /// value to JSON, keeping SQL NULL and leaving an argument that is already
    /// JSON alone.
    ToJson,
    /// Population variance: var_pop(x) becomes avg(x*x) - avg(x)*avg(x).
    VarPop,
    /// Population standard deviation: stddev_pop(x) becomes sqrt(var_pop(x)).
    StddevPop,
    /// Sample variance: var_samp(x) becomes
    /// (sum(x*x) - sum(x)*sum(x)/count(x)) / (count(x) - 1).
    /// PG `variance` aliases to this.
    VarSamp,
    /// Sample standard deviation: stddev_samp(x) becomes sqrt(var_samp(x)).
    /// PG `stddev` aliases to this.
    StddevSamp,
    /// Population covariance: covar_pop(x, y) becomes
    /// avg(x*y) - avg(x) * avg(y).
    CovarPop,
    /// Sample covariance: covar_samp(x, y) becomes
    /// (sum(x*y) - sum(x) * sum(y) / count(*)) / (count(*) - 1).
    CovarSamp,
    /// Pearson correlation: corr(x, y) becomes
    /// covar_pop(x, y) / (sqrt(var_pop(x)) * sqrt(var_pop(y))).
    Corr,
    /// No translation needed
    PassThrough,
    /// An array function whose body is rewritten over `json_each` /
    /// `json_group_array`. See [`super::array`].
    Array(ArrayFunction),
    /// `round(x, n)` over a value held as minor units, which has to move to
    /// scale `n` and back rather than round the integer count.
    NumericRound,
    /// `string_agg`, whose separator argument SQLite's `group_concat` refuses
    /// to take alongside DISTINCT.
    StringAgg,
    /// `char_length`/`character_length`, which PostgreSQL defines over text
    /// alone where SQLite's `length` also accepts a blob and counts bytes.
    CharLength {
        /// The spelling as written, so the error names the function the query
        /// used.
        label: &'static str,
    },
    /// `quote_literal`/`quote_nullable`, which agree on everything but NULL.
    Quote {
        /// True for `quote_nullable`, which answers the four characters `NULL`
        /// where `quote_literal` answers SQL NULL.
        nullable: bool,
    },
    /// `json_typeof`/`jsonb_typeof`, whose answers are renamed onto SQLite's
    /// `json_type` vocabulary.
    JsonTypeof,
    /// `json_agg`/`jsonb_agg`, which nest a JSON element where
    /// `json_group_array` would quote it, and answer NULL over no rows where it
    /// answers an empty array.
    JsonAgg,
    /// `greatest`/`least`, which ignore NULL arguments where SQLite's scalar
    /// `MAX`/`MIN` return NULL as soon as one argument is NULL.
    Extremum {
        /// `MAX` for `greatest`, `MIN` for `least`.
        greatest: bool,
    },
    /// `cbrt(x)` translated to `pow(x, (1.0 / 3.0))` when math functions are
    /// available.
    ToCbrt,
}

/// Simple name-only renames: `(pg_name, sqlite_name)`.
/// Checked before the main match for a compact fast path.
const FORWARD_RENAMES: &[(&str, &str)] = &[
    // greatest and least are NOT renames: SQLite's scalar MAX and MIN return
    // NULL when any argument is NULL. See `FunctionTranslation::Extremum`.
    // json_agg and jsonb_agg are NOT renames: a JSON column is TEXT in SQLite
    // and would be quoted rather than nested. See `FunctionTranslation::JsonAgg`.
    // json_typeof and jsonb_typeof are NOT renames: json_type answers over a
    // different vocabulary. See `FunctionTranslation::JsonTypeof`.
    // quote_literal and quote_nullable are NOT renames: they differ on NULL,
    // and both quote a number where SQLite's quote does not. See
    // `FunctionTranslation::Quote`.
    // char_length and character_length are NOT renames: PostgreSQL defines them
    // over text alone, while SQLite's length accepts a blob and counts its
    // bytes. See `FunctionTranslation::CharLength`.
    // string_agg is NOT a rename: SQLite takes no separator argument beside
    // DISTINCT. See `FunctionTranslation::StringAgg`.
    ("strpos", "INSTR"),
    ("chr", "char"),
    ("json_object_agg", "json_group_object"),
    ("jsonb_object_agg", "json_group_object"),
    ("json_build_array", "json_array"),
    ("json_build_object", "json_object"),
    ("btrim", "trim"),
    ("jsonb_array_length", "json_array_length"),
    ("version", "sqlite_version"),
    // to_json and to_jsonb are NOT renames: `json()` reads its argument as JSON
    // where they convert a value into JSON. See `FunctionTranslation::ToJson`.
    // jsonb_set and jsonb_insert are NOT renames: their path and value
    // arguments need translating too. See `FunctionTranslation::JsonSet`.
    ("jsonb_each", "json_each"),
    ("json_each_text", "json_each"),
    ("jsonb_each_text", "json_each"),
    ("ascii", "unicode"),
];

/// Builds a NULL-ignoring `MAX`/`MIN` over `arguments`.
///
/// Each slot is a `coalesce` starting at one argument and wrapping around, so
/// a slot is NULL only when every argument is, and slot `i` is `arguments[i]`
/// itself whenever that is not NULL. The values reaching `MAX` are therefore
/// exactly the non-NULL arguments, which is PostgreSQL's rule.
///
/// A `VALUES` subquery would be shorter but cannot see the outer query's
/// columns and is rejected inside an index expression, where this form works.
fn null_ignoring_extremum(
    arguments: &[Expr],
    greatest: bool,
    label: &str,
) -> Result<Expr, crate::errors::Error> {
    let Some((first, rest)) = arguments.split_first() else {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "{label} needs at least one argument"
        )));
    };

    // A single argument is already its own extremum, and SQLite's one-argument
    // `MAX` is the AGGREGATE, which would collapse the rows instead.
    if rest.is_empty() {
        return Ok(first.clone());
    }

    let rotations = (0..arguments.len())
        .map(|start| {
            let rotated =
                arguments.iter().cycle().skip(start).take(arguments.len()).cloned().collect();
            simple_function_expr("coalesce", rotated, None)
        })
        .collect();

    Ok(simple_function_expr(if greatest { "MAX" } else { "MIN" }, rotations, None))
}

/// Builds PostgreSQL's `trunc(x, n)`, which truncates toward zero, out of
/// SQLite's parts.
///
/// The shape is `CAST(round(x * 10^n, 9) AS INTEGER) / 10^n`, since a CAST to
/// INTEGER truncates toward zero for both signs.
///
/// The inner `round` absorbs binary representation noise: without it
/// `1.15 * 100` is 114.99999999999999 and `trunc(1.15, 2)` answers 1.14.
///
/// A literal scale is folded into a literal factor, keeping `pow` out of the
/// emitted SQL. A computed scale needs `pow`, which ships only under
/// `SQLITE_ENABLE_MATH_FUNCTIONS`, and is refused without it.
fn truncate_to_scale(
    x: Expr,
    scale: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let factor = match literal_integer(scale) {
        Some(digits) => number_literal(&format!("{:.10}", 10f64.powi(digits))),
        None if options.are_math_functions_available() => {
            simple_function_expr(
                "pow",
                vec![number_literal("10"), scale.translate(schema, options)?],
                None,
            )
        }
        None => {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "trunc(x, n) with a computed scale needs pow(), which is a math function that \
                 ships only under SQLITE_ENABLE_MATH_FUNCTIONS. Declare that build, or write the \
                 scale as a literal."
                    .to_string(),
            ));
        }
    };

    let scaled = Expr::BinaryOp {
        left: Box::new(x),
        op: BinaryOperator::Multiply,
        right: Box::new(factor.clone()),
    };
    let truncated = Expr::Cast {
        expr: Box::new(simple_function_expr("round", vec![scaled, number_literal("9")], None)),
        data_type: DataType::Integer(None),
        format: None,
        kind: CastKind::Cast,
    };

    Ok(Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(truncated),
        op: BinaryOperator::Divide,
        right: Box::new(factor),
    })))
}

/// The value of `expr` when it is an integer literal, with an optional sign.
fn literal_integer(expr: &Expr) -> Option<i32> {
    match expr {
        Expr::Value(ValueWithSpan { value: Value::Number(digits, _), .. }) => digits.parse().ok(),
        Expr::UnaryOp { op: UnaryOperator::Minus, expr } => literal_integer(expr).map(|n| -n),
        Expr::Nested(inner) => literal_integer(inner),
        _ => None,
    }
}

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

    // Array functions needing a rewritten body over `json_each`.
    if let Some(kind) = ArrayFunction::from_name(original_name.as_str()) {
        return FunctionTranslation::Array(kind);
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
        // `array_agg` and `cardinality` need only a rename: `json_group_array`
        // accumulates the same way, and `json_array_length` counts the same
        // way, including reporting zero for an empty array.
        "array_agg" | "cardinality" => {
            if array::is_json_array_representation(options) {
                let target =
                    if original_name == "array_agg" { "json_group_array" } else { "json_array_length" };
                FunctionTranslation::Rename(target.to_string())
            } else {
                FunctionTranslation::Unsupported(array::representation_required_message(&format!(
                    "{original_name}()"
                )))
            }
        }
        "bit_and" | "bit_or" => FunctionTranslation::Unsupported(format!(
            "{original_name} is not supported as an aggregate in SQLite. \
             Consider loading a custom extension or rewriting with bitwise expressions."
        )),
        "stddev_pop" => {
            if options.are_math_functions_available() {
                FunctionTranslation::StddevPop
            } else {
                FunctionTranslation::Unsupported(
                    "stddev_pop() needs sqrt() to compute the closed-form result, \
                     which is not available in standard SQLite. \
                     Call with_math_functions_available() on Pg2SqliteOptions when \
                     your SQLite build includes SQLITE_ENABLE_MATH_FUNCTIONS."
                        .to_string(),
                )
            }
        }
        "stddev" | "stddev_samp" => {
            if options.are_math_functions_available() {
                FunctionTranslation::StddevSamp
            } else {
                FunctionTranslation::Unsupported(
                    "stddev/stddev_samp() needs sqrt() to compute the closed-form result, \
                     which is not available in standard SQLite. \
                     Call with_math_functions_available() on Pg2SqliteOptions when \
                     your SQLite build includes SQLITE_ENABLE_MATH_FUNCTIONS."
                        .to_string(),
                )
            }
        }
        "var_pop" => FunctionTranslation::VarPop,
        "variance" | "var_samp" => FunctionTranslation::VarSamp,
        "corr" => {
            if options.are_math_functions_available() {
                FunctionTranslation::Corr
            } else {
                FunctionTranslation::Unsupported(
                    "corr() needs sqrt() to compute the closed-form result, \
                     which is not available in standard SQLite. \
                     Call with_math_functions_available() on Pg2SqliteOptions when \
                     your SQLite build includes SQLITE_ENABLE_MATH_FUNCTIONS."
                        .to_string(),
                )
            }
        }
        "covar_pop" => FunctionTranslation::CovarPop,
        "covar_samp" => FunctionTranslation::CovarSamp,
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
        // round over a NUMERIC needs the column's scale, so it is decided at
        // emission where the schema is in hand.
        "round" => FunctionTranslation::NumericRound,
        // string_agg: SQLite takes no separator argument beside DISTINCT.
        "string_agg" => FunctionTranslation::StringAgg,
        // char_length / character_length: text only in PostgreSQL.
        "char_length" => FunctionTranslation::CharLength { label: "char_length" },
        "character_length" => FunctionTranslation::CharLength { label: "character_length" },
        // quote_literal / quote_nullable: they differ only on NULL.
        "quote_literal" => FunctionTranslation::Quote { nullable: false },
        "quote_nullable" => FunctionTranslation::Quote { nullable: true },
        // json_typeof / jsonb_typeof: json_type names the types differently.
        "json_typeof" | "jsonb_typeof" => FunctionTranslation::JsonTypeof,
        // json_agg / jsonb_agg: a JSON element needs parsing, not quoting.
        "json_agg" | "jsonb_agg" => FunctionTranslation::JsonAgg,
        // greatest / least ignore NULLs, MAX / MIN do not.
        "greatest" => FunctionTranslation::Extremum { greatest: true },
        "least" => FunctionTranslation::Extremum { greatest: false },
        // to_json / to_jsonb: a conversion, not a reinterpretation.
        "to_json" | "to_jsonb" => FunctionTranslation::ToJson,
        // jsonb_set / jsonb_insert: path and value both need converting.
        "jsonb_set" => FunctionTranslation::JsonSet { insert: false },
        "jsonb_insert" => FunctionTranslation::JsonSet { insert: true },
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
        // reverse: not in standard SQLite; passing through would cause a runtime
        // crash on "no such function: reverse".
        "reverse" => FunctionTranslation::Unsupported(
            "reverse is not available in standard SQLite. \
             Consider using application-level string reversal or a custom extension."
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
        // unnest: a set-returning function, valid only in a FROM clause, where
        // `shared_helpers` rewrites it to `json_each`.
        "unnest" => FunctionTranslation::Unsupported(
            "unnest() is only translatable in a FROM clause, where it becomes json_each(). \
             In a SELECT list it returns a set, which SQLite cannot express as a scalar."
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

        // Math functions that require SQLITE_ENABLE_MATH_FUNCTIONS. When the
        // option is declared, scalars pass through and power/cbrt get faithful
        // translations. When it is not declared, all are rejected with a clear
        // message pointing to the opt-in.
        "log" | "ln" | "exp" | "sqrt" | "log10" | "pow" | "power" | "cbrt" => {
            if options.are_math_functions_available() {
                match original_name.as_str() {
                    "power" => FunctionTranslation::Rename("pow".to_string()),
                    "cbrt" => FunctionTranslation::ToCbrt,
                    _ => FunctionTranslation::PassThrough,
                }
            } else {
                FunctionTranslation::Unsupported(format!(
                    "{original_name}() is not available in standard SQLite. \
                     Call with_math_functions_available() on Pg2SqliteOptions when \
                     your SQLite build includes SQLITE_ENABLE_MATH_FUNCTIONS."
                ))
            }
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
        // ROW(a, b) is a row-value constructor. SQLite has no row type.
        // Tuple comparison (a, b) = (c, d) is supported via Expr::Tuple and already works.
        "row" => FunctionTranslation::Unsupported(
            "ROW(a, b) as a standalone value is not supported in SQLite (no row type). \
             For tuple comparison, use (a, b) = (c, d) instead."
                .to_string(),
        ),

        // Array functions with no faithful json1 form. `json_each` hands a
        // nested element back as JSON text, so anything that inspects or
        // rebuilds dimensions cannot be answered correctly.
        "array_cat" | "array_prepend" => FunctionTranslation::Unsupported(array::no_json_message(
            &format!("{original_name}()"),
            "Concatenating JSON arrays would have to re-encode every element and SQLite has no \
             json_concat(). Build the combined array in the application, or insert elements one \
             at a time with json_insert(a, '$[#]', v).",
        )),
        "array_dims" | "array_ndims" => {
            FunctionTranslation::Unsupported(array::no_json_message(
                &format!("{original_name}()"),
                "A JSON array carries no dimension metadata; only one-dimensional arrays are \
                 represented.",
            ))
        }
        "array_fill" => FunctionTranslation::Unsupported(array::no_json_message(
            "array_fill()",
            "Filling an array needs a row generator, and SQLite has no generate_series() in the \
             core build. Build the array in the application.",
        )),

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

        _ => maybe_sqlitegis_passthrough(&original_name, args, options),
    }
}

/// When SQLiteGIS translation is enabled, validate `ST_*`-shaped calls against
/// the catalog mirrored from the extension (`super::postgis`). Names in the
/// catalog with a matching arity pass through, everything else errors. With
/// SQLiteGIS off this is a no-op and unknown functions keep their pre-existing
/// passthrough behavior.
fn maybe_sqlitegis_passthrough(
    name: &str,
    args: &FunctionArguments,
    options: &Pg2SqliteOptions,
) -> FunctionTranslation {
    if !options.is_sqlitegis_enabled() {
        return FunctionTranslation::PassThrough;
    }
    let Some(arity) = function_arg_count(args) else {
        return FunctionTranslation::PassThrough;
    };
    if postgis::is_sqlitegis_function(name, arity) {
        return FunctionTranslation::PassThrough;
    }
    let known_arities = postgis::sqlitegis_function_arities(name);
    if !known_arities.is_empty() {
        return FunctionTranslation::Unsupported(format!(
            "{name}/{arity} is not in the SQLiteGIS catalog; SQLiteGIS implements arities \
             {known_arities:?} for this name."
        ));
    }
    if postgis::is_postgis_shaped_name(name) {
        return FunctionTranslation::Unsupported(format!(
            "{name}() looks like a PostGIS function but is not implemented by the SQLiteGIS \
             extension, see https://github.com/LucaCappelletti94/sqlitegis for the supported list."
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

/// True when `expr` already carries JSON, so `to_json` has nothing to convert.
///
/// Recognised syntactically, which covers what this translator itself emits and
/// what a migration writes inline: an array literal, which becomes JSON text
/// under the JSON array representation, and a call to a function that returns
/// JSON.
///
/// It cannot see through a bare column reference, so an array or `json` COLUMN
/// takes the `json_quote` path and is quoted into a string, which is wrong and
/// needs the column's declared type to fix. Tracked as R89.
fn is_already_json(expr: &Expr) -> bool {
    const JSON_VALUED: [&str; 10] = [
        "json",
        "jsonb",
        "json_array",
        "json_object",
        "json_group_array",
        "json_group_object",
        "json_quote",
        "to_json",
        "json_agg",
        "jsonb_agg",
    ];

    match expr {
        Expr::Array(_) => true,
        Expr::Function(func) => {
            func.name.0.last().and_then(ObjectNamePart::as_ident).is_some_and(|name| {
                JSON_VALUED.iter().any(|json| name.value.eq_ignore_ascii_case(json))
            })
        }
        Expr::Nested(inner) => is_already_json(inner),
        _ => false,
    }
}

/// Renames SQLite's `json_type` answer onto PostgreSQL's `json_typeof` one.
///
/// Those eight names are the whole of SQLite's domain, so all are listed and
/// the `CASE` needs no `ELSE`. The missing `ELSE` yields NULL, which is also
/// the right answer for a NULL argument. Falling through to an `ELSE` instead
/// would have to name the argument twice, since a `CASE` with an operand
/// cannot refer to it again.
fn postgres_json_type_name(sqlite_type: Expr) -> Expr {
    const VOCABULARY: [(&str, &str); 8] = [
        ("text", "string"),
        ("integer", "number"),
        ("real", "number"),
        ("true", "boolean"),
        ("false", "boolean"),
        ("null", "null"),
        ("object", "object"),
        ("array", "array"),
    ];

    Expr::Case {
        case_token: AttachedToken::empty(),
        end_token: AttachedToken::empty(),
        operand: Some(Box::new(sqlite_type)),
        conditions: VOCABULARY
            .iter()
            .map(|&(sqlite, postgres)| {
                CaseWhen { condition: string_literal(sqlite), result: string_literal(postgres) }
            })
            .collect(),
        else_result: None,
    }
}

/// Swaps the first positional argument of an already translated argument list,
/// leaving `ORDER BY`, `DISTINCT`, and the rest of the clauses in place.
fn replace_first_argument(args: &mut FunctionArguments, replacement: Expr) {
    if let FunctionArguments::List(list) = args
        && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(first))) = list.args.first_mut()
    {
        *first = replacement;
    }
}

/// True when `expr` names a column whose declared type is not a text type.
///
/// False for anything with no declared type to consult, a literal or a computed
/// expression, since those are not the case this guards and refusing them would
/// refuse valid PostgreSQL.
fn resolves_to_non_textual_column(expr: &Expr, schema: &ParserDB) -> bool {
    const TEXTUAL: [&str; 7] =
        ["text", "varchar", "character varying", "char", "bpchar", "citext", "name"];

    every_declared_type_matches(expr, schema, |declared| {
        let lowered = declared.to_ascii_lowercase();
        !TEXTUAL.iter().any(|textual| lowered.starts_with(textual))
    })
}

/// True when `expr` carries a JSON document, either by its shape or by the
/// declared type of the column it names.
///
/// A `json` or `jsonb` column becomes TEXT in SQLite and is otherwise
/// indistinguishable from a string column, so the declared type is the only
/// thing that separates a document from its own text. An unqualified name is
/// accepted only when every column with that name in the schema agrees, since
/// guessing between the two is wrong half the time in either direction.
fn carries_json(expr: &Expr, schema: &ParserDB) -> bool {
    is_already_json(expr)
        || every_declared_type_matches(expr, schema, |data_type| {
            matches!(data_type.to_ascii_lowercase().as_str(), "json" | "jsonb")
        })
}

/// The SQLite function that matches PostgreSQL's fourth argument.
///
/// Measured against both databases rather than assumed. `jsonb_set`'s
/// `create_if_missing` defaults to true and maps to `json_set`, while `false`
/// maps to `json_replace`, which leaves a missing path untouched exactly as
/// PostgreSQL does. `jsonb_insert`'s fourth argument is `insert_after` rather
/// than `create_if_missing`, and it places the value after an array element,
/// which SQLite cannot express at all.
fn json_set_target_function(
    insert: bool,
    flag: Option<&Expr>,
    label: &str,
) -> Result<&'static str, crate::errors::Error> {
    let flag = match flag {
        None => None,
        Some(Expr::Value(ValueWithSpan { value: Value::Boolean(flag), .. })) => Some(*flag),
        Some(other) => {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "{label} needs its fourth argument to be a literal true or false to choose the \
                 matching SQLite function, and {other} is decided at run time."
            )));
        }
    };

    match (insert, flag) {
        (false, None | Some(true)) => Ok("json_set"),
        (false, Some(false)) => Ok("json_replace"),
        (true, None | Some(false)) => Ok("json_insert"),
        (true, Some(true)) => Err(crate::errors::Error::UnsupportedSQLiteFeature(
            "jsonb_insert with insert_after cannot be translated: it places the value after an \
                 array element, and SQLite's json_insert only fills a path that is absent."
                .to_string(),
        )),
    }
}

/// Converts PostgreSQL's `text[]` path to the JSONPath string SQLite takes,
/// so `'{a,b}'` and `ARRAY['a','b']` both become `$.a.b`.
///
/// A numeric element is refused rather than guessed. PostgreSQL decides at run
/// time whether it indexes an array or names an object key, verified both ways
/// against PostgreSQL 16: `'{arr,0}'` set element 0 of an array, and `'{0}'`
/// set the key `"0"` of an object. JSONPath has to commit to one at translation
/// time, and picking wrong writes to the wrong place silently.
fn json_path_from_text_array(path: &Expr, label: &str) -> Result<String, crate::errors::Error> {
    let elements = match path {
        Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(literal), .. }) => {
            let trimmed = literal.trim();
            let inner = trimmed
                .strip_prefix('{')
                .and_then(|rest| rest.strip_suffix('}'))
                .ok_or_else(|| json_path_not_literal(label, path))?;
            inner.split(',').map(|element| element.trim().to_owned()).collect::<Vec<_>>()
        }
        Expr::Array(array) => {
            array
                .elem
                .iter()
                .map(|element| {
                    match element {
                        Expr::Value(ValueWithSpan {
                            value: Value::SingleQuotedString(key),
                            ..
                        }) => Ok(key.clone()),
                        other => Err(json_path_not_literal(label, other)),
                    }
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        other => return Err(json_path_not_literal(label, other)),
    };

    if elements.iter().any(String::is_empty) {
        return Err(json_path_not_literal(label, path));
    }

    let mut json_path = String::from("$");
    for element in &elements {
        if element.parse::<i64>().is_ok() {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "{label} cannot translate the path element {element}, because PostgreSQL decides \
                 at run time whether it indexes an array or names an object key, and JSONPath has \
                 to choose one. Use json_set with an explicit $.a[0] path against the SQLite \
                 database instead."
            )));
        }
        if element.contains('"') {
            return Err(json_path_not_literal(label, path));
        }
        // A key that is not a bare identifier has to be quoted in JSONPath.
        if element.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !element.starts_with(|c: char| c.is_ascii_digit())
        {
            json_path.push('.');
            json_path.push_str(element);
        } else {
            json_path.push_str(".\"");
            json_path.push_str(element);
            json_path.push('"');
        }
    }

    Ok(json_path)
}

fn json_path_not_literal(label: &str, path: &Expr) -> crate::errors::Error {
    crate::errors::Error::UnsupportedSQLiteFeature(format!(
        "{label} needs a literal text[] path such as '{{a,b}}' or ARRAY['a','b'] so it can be \
         converted to the JSONPath SQLite takes, and {path} cannot be converted at translation \
         time."
    ))
}

/// Extract and translate the single argument of an aggregate like
/// `var_pop(x)` or `stddev_pop(x)`. Errors when the call has no positional
/// argument expression.
fn single_aggregate_arg(
    args: &FunctionArguments,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
    func_name: &str,
) -> Result<Expr, crate::errors::Error> {
    let exprs = function_argument_exprs(args);
    let first = exprs.into_iter().next().ok_or_else(|| {
        crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "{func_name} requires one argument expression"
        ))
    })?;
    first.translate(schema, options)
}

/// Build the population-variance closed form `avg(x*x) - avg(x) * avg(x)`.
/// `x` is cloned twice for the squared term and twice more for the mean
/// product, so the caller passes the already-translated expression.
fn var_pop_closed_form(x: Expr) -> Expr {
    let x_squared = Expr::BinaryOp {
        left: Box::new(x.clone()),
        op: BinaryOperator::Multiply,
        right: Box::new(x.clone()),
    };
    let avg_x_squared = simple_function_expr("avg", vec![x_squared], None);
    let avg_x = simple_function_expr("avg", vec![x], None);
    let avg_x_times_avg_x = Expr::BinaryOp {
        left: Box::new(avg_x.clone()),
        op: BinaryOperator::Multiply,
        right: Box::new(avg_x),
    };
    Expr::BinaryOp {
        left: Box::new(avg_x_squared),
        op: BinaryOperator::Minus,
        right: Box::new(avg_x_times_avg_x),
    }
}

/// Build the sample-variance closed form
/// `(sum(x*x) - sum(x) * sum(x) / count(x)) / (count(x) - 1)`. Numerator
/// and denominator are wrapped in `Nested` so the rendered SQL keeps the
/// correct precedence around the outer division.
fn var_samp_closed_form(x: Expr) -> Expr {
    let x_squared = Expr::BinaryOp {
        left: Box::new(x.clone()),
        op: BinaryOperator::Multiply,
        right: Box::new(x.clone()),
    };
    let sum_x_squared = simple_function_expr("sum", vec![x_squared], None);
    let sum_x = simple_function_expr("sum", vec![x.clone()], None);
    let count_x = simple_function_expr("count", vec![x], None);

    let sum_x_times_sum_x = Expr::BinaryOp {
        left: Box::new(sum_x.clone()),
        op: BinaryOperator::Multiply,
        right: Box::new(sum_x),
    };
    let correction = Expr::BinaryOp {
        left: Box::new(sum_x_times_sum_x),
        op: BinaryOperator::Divide,
        right: Box::new(count_x.clone()),
    };
    let numerator = Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(sum_x_squared),
        op: BinaryOperator::Minus,
        right: Box::new(correction),
    }));
    let denominator = Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(count_x),
        op: BinaryOperator::Minus,
        right: Box::new(number_literal("1")),
    }));
    Expr::BinaryOp {
        left: Box::new(numerator),
        op: BinaryOperator::Divide,
        right: Box::new(denominator),
    }
}

/// Extract and translate exactly two argument expressions, as required
/// by bivariate aggregates such as `covar_pop(x, y)` or `corr(x, y)`.
fn two_aggregate_args(
    args: &FunctionArguments,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
    func_name: &str,
) -> Result<(Expr, Expr), crate::errors::Error> {
    let exprs = function_argument_exprs(args);
    if exprs.len() < 2 {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "{func_name} requires two argument expressions"
        )));
    }
    let mut iter = exprs.into_iter();
    let x = iter.next().unwrap();
    let y = iter.next().unwrap();
    Ok((x.translate(schema, options)?, y.translate(schema, options)?))
}

/// Build the population-covariance closed form
/// `avg(x*y) - avg(x) * avg(y)`.
fn covar_pop_closed_form(x: &Expr, y: &Expr) -> Expr {
    let (x, y) = (paired_with(x, y), paired_with(y, x));
    let xy = Expr::BinaryOp {
        left: Box::new(x.clone()),
        op: BinaryOperator::Multiply,
        right: Box::new(y.clone()),
    };
    let avg_xy = simple_function_expr("avg", vec![xy], None);
    let mean_x = simple_function_expr("avg", vec![x], None);
    let mean_y = simple_function_expr("avg", vec![y], None);
    let avg_x_times_avg_y = Expr::BinaryOp {
        left: Box::new(mean_x),
        op: BinaryOperator::Multiply,
        right: Box::new(mean_y),
    };
    Expr::BinaryOp {
        left: Box::new(avg_xy),
        op: BinaryOperator::Minus,
        right: Box::new(avg_x_times_avg_y),
    }
}

/// `CASE WHEN <partner> IS NOT NULL THEN <value> END`, so an aggregate over the
/// result sees only the rows where both inputs are present.
///
/// PostgreSQL's bivariate aggregates are defined over the complete pairs, so
/// every marginal inside one has to be taken over the same row set as the joint
/// term. Taking `avg(x)` over its own non-NULL rows and `avg(x*y)` over the
/// pairs breaks the identity these closed forms rest on.
fn paired_with(value: &Expr, partner: &Expr) -> Expr {
    case_when(Expr::IsNotNull(Box::new(partner.clone())), value.clone(), None)
}

/// Build the sample-covariance closed form
/// `(sum(x*y) - sum(x) * sum(y) / count(x*y)) / (count(x*y) - 1)`.
///
/// The pair count is `count(x*y)` rather than `count(*)`: the product is NULL
/// whenever either input is, so counting it counts exactly the rows
/// PostgreSQL averages over, where `count(*)` counted every row in the group.
fn covar_samp_closed_form(x: &Expr, y: &Expr) -> Expr {
    let (x, y) = (paired_with(x, y), paired_with(y, x));
    let xy = Expr::BinaryOp {
        left: Box::new(x.clone()),
        op: BinaryOperator::Multiply,
        right: Box::new(y.clone()),
    };
    let sum_xy = simple_function_expr("sum", vec![xy.clone()], None);
    let total_x = simple_function_expr("sum", vec![x], None);
    let total_y = simple_function_expr("sum", vec![y], None);
    let count_star = simple_function_expr("count", vec![xy], None);

    let sum_x_times_sum_y = Expr::BinaryOp {
        left: Box::new(total_x),
        op: BinaryOperator::Multiply,
        right: Box::new(total_y),
    };
    let correction = Expr::BinaryOp {
        left: Box::new(sum_x_times_sum_y),
        op: BinaryOperator::Divide,
        right: Box::new(count_star.clone()),
    };
    let numerator = Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(sum_xy),
        op: BinaryOperator::Minus,
        right: Box::new(correction),
    }));
    let denominator = Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(count_star),
        op: BinaryOperator::Minus,
        right: Box::new(number_literal("1")),
    }));
    Expr::BinaryOp {
        left: Box::new(numerator),
        op: BinaryOperator::Divide,
        right: Box::new(denominator),
    }
}

/// Build the Pearson correlation closed form
/// `covar_pop(x, y) / (sqrt(var_pop(x)) * sqrt(var_pop(y)))`.
///
/// The two deviations are paired here as well, which is not automatic: the
/// numerator pairs itself inside `covar_pop_closed_form`, but `var_pop` of a
/// bare column would still spread over rows whose partner is NULL, and the
/// ratio of terms taken over different row sets is not a correlation.
fn corr_closed_form(x: &Expr, y: &Expr) -> Expr {
    let numerator = covar_pop_closed_form(x, y);
    let stddev_x = simple_function_expr("sqrt", vec![var_pop_closed_form(paired_with(x, y))], None);
    let stddev_y = simple_function_expr("sqrt", vec![var_pop_closed_form(paired_with(y, x))], None);
    let denominator = Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(stddev_x),
        op: BinaryOperator::Multiply,
        right: Box::new(stddev_y),
    }));
    Expr::BinaryOp {
        left: Box::new(Expr::Nested(Box::new(numerator))),
        op: BinaryOperator::Divide,
        right: Box::new(denominator),
    }
}

/// `left(s, n)`: the first `n` characters, or all but the last `|n|` when `n`
/// is negative.
///
/// SQLite's `substr(s, 1, n)` returns the empty string for a negative length,
/// so the negative case has to be converted into a length measured from the
/// front. `n` is read twice, which is only observable for a volatile count.
fn left_closed_form(s: Expr, n: Expr) -> Expr {
    let from_end = simple_function_expr(
        "max",
        vec![
            Expr::BinaryOp {
                left: Box::new(simple_function_expr("length", vec![s.clone()], None)),
                op: BinaryOperator::Plus,
                right: Box::new(n.clone()),
            },
            integer_literal(0),
        ],
        None,
    );
    let length = case_when(
        Expr::BinaryOp {
            left: Box::new(n.clone()),
            op: BinaryOperator::Lt,
            right: Box::new(integer_literal(0)),
        },
        from_end,
        Some(n),
    );
    simple_function_expr("substr", vec![s, integer_literal(1), length], None)
}

/// `right(s, n)`: the last `n` characters, or all but the first `|n|` when `n`
/// is negative.
///
/// SQLite's `substr(s, -n)` gives the last `n` characters only for a positive
/// `n`. For a negative `n` it reads as a positive offset from the start, which
/// is off by one from PostgreSQL, and for `n = 0` it returns the whole string
/// rather than nothing, so both cases are computed as an explicit start offset.
fn right_closed_form(s: Expr, n: Expr) -> Expr {
    let drop_from_front = Expr::BinaryOp {
        left: Box::new(integer_literal(1)),
        op: BinaryOperator::Minus,
        right: Box::new(n.clone()),
    };
    let last_n = simple_function_expr(
        "max",
        vec![
            Expr::BinaryOp {
                left: Box::new(Expr::BinaryOp {
                    left: Box::new(simple_function_expr("length", vec![s.clone()], None)),
                    op: BinaryOperator::Minus,
                    right: Box::new(n.clone()),
                }),
                op: BinaryOperator::Plus,
                right: Box::new(integer_literal(1)),
            },
            integer_literal(1),
        ],
        None,
    );
    let start = case_when(
        Expr::BinaryOp {
            left: Box::new(n),
            op: BinaryOperator::Lt,
            right: Box::new(integer_literal(0)),
        },
        drop_from_front,
        Some(last_n),
    );
    simple_function_expr("substr", vec![s, start], None)
}

/// Wrap an expression with COALESCE(expr, '') to handle NULL semantics.
///
/// PostgreSQL's CONCAT ignores NULL arguments. SQLite's `||` propagates them.
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
        let prefix = case_when(has_prior_non_null, separator.clone(), Some(string_literal("")));
        Expr::BinaryOp {
            left: Box::new(prefix),
            op: BinaryOperator::StringConcat,
            right: Box::new(value.clone()),
        }
    } else {
        value.clone()
    };

    // PostgreSQL CONCAT_WS skips NULL values entirely.
    case_when(Expr::IsNull(Box::new(value)), string_literal(""), Some(prefixed_value))
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

                // Map PostgreSQL truncation granularities to strftime format
                // strings. The format zeros out the sub-granularity components
                // rather than dropping them: PostgreSQL's date_trunc always
                // answers a full timestamp, so a coarse unit that stopped at the
                // date would never compare equal to a stored one, which this
                // crate writes as TEXT `YYYY-MM-DD HH:MM:SS`.
                let format_str = match field_str.as_str() {
                    "second" | "seconds" => "%Y-%m-%d %H:%M:%S",
                    "minute" | "minutes" => "%Y-%m-%d %H:%M:00",
                    "hour" | "hours" => "%Y-%m-%d %H:00:00",
                    "day" | "days" => "%Y-%m-%d 00:00:00",
                    "month" | "months" => "%Y-%m-01 00:00:00",
                    "year" | "years" => "%Y-01-01 00:00:00",
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
                let exprs = extract_exactly(&func.args, 2, "left")?;
                let s = exprs[0].translate(schema, options)?;
                let n = exprs[1].translate(schema, options)?;
                Ok(left_closed_form(s, n))
            }
            FunctionTranslation::ToSubstrRight => {
                let exprs = extract_exactly(&func.args, 2, "right")?;
                let s = exprs[0].translate(schema, options)?;
                let n = exprs[1].translate(schema, options)?;
                Ok(right_closed_form(s, n))
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
                        })
                    }
                    2 => {
                        let x = exprs[0].translate(schema, options)?;
                        truncate_to_scale(x, exprs[1], schema, options)
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
            FunctionTranslation::JsonSet { insert } => {
                let exprs = function_argument_exprs(&func.args);
                let label = if insert { "jsonb_insert" } else { "jsonb_set" };
                let ([target, path, value] | [target, path, value, _]) = exprs.as_slice() else {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                        "{label} takes a document, a path, a value, and optionally a flag, so {} \
                         arguments cannot be translated.",
                        exprs.len()
                    )));
                };

                let sqlite_name = json_set_target_function(insert, exprs.get(3).copied(), label)?;
                // The value is `jsonb` in PostgreSQL, so `'2'` means the number
                // 2. SQLite reads a bare text argument as a string, which would
                // store `"2"`, so it is wrapped rather than passed along.
                let value =
                    simple_function_expr("json", vec![value.translate(schema, options)?], None);
                Ok(simple_function_expr(
                    sqlite_name,
                    vec![
                        target.translate(schema, options)?,
                        string_literal(&json_path_from_text_array(path, label)?),
                        value,
                    ],
                    None,
                ))
            }
            FunctionTranslation::ToJson => {
                let exprs = extract_exactly(&func.args, 1, "to_json")?;
                let argument = exprs[0];
                let translated = argument.translate(schema, options)?;

                // An argument that is already JSON needs reading, not
                // converting: quoting it would turn the document into a string.
                if is_already_json(argument) {
                    return Ok(simple_function_expr("json", vec![translated], None));
                }

                // `json_quote` renders SQL NULL as the JSON text `null` where
                // PostgreSQL yields SQL NULL, and it produces that bare `null`
                // for no other input, so NULLIF restores it while evaluating
                // the argument once.
                Ok(simple_function_expr(
                    "NULLIF",
                    vec![
                        simple_function_expr("json_quote", vec![translated], None),
                        string_literal("null"),
                    ],
                    None,
                ))
            }
            FunctionTranslation::VarPop => {
                let x = single_aggregate_arg(&func.args, schema, options, "var_pop")?;
                Ok(var_pop_closed_form(x))
            }
            FunctionTranslation::StddevPop => {
                let x = single_aggregate_arg(&func.args, schema, options, "stddev_pop")?;
                Ok(simple_function_expr("sqrt", vec![var_pop_closed_form(x)], None))
            }
            FunctionTranslation::VarSamp => {
                let x = single_aggregate_arg(&func.args, schema, options, "var_samp")?;
                Ok(var_samp_closed_form(x))
            }
            FunctionTranslation::StddevSamp => {
                let x = single_aggregate_arg(&func.args, schema, options, "stddev_samp")?;
                Ok(simple_function_expr("sqrt", vec![var_samp_closed_form(x)], None))
            }
            FunctionTranslation::Extremum { greatest } => {
                let exprs = function_argument_exprs(&func.args);
                let arguments = exprs
                    .iter()
                    .map(|expr| expr.translate(schema, options))
                    .collect::<Result<Vec<_>, _>>()?;
                null_ignoring_extremum(
                    &arguments,
                    greatest,
                    if greatest { "greatest" } else { "least" },
                )
            }
            FunctionTranslation::NumericRound => {
                let exprs = function_argument_exprs(&func.args);
                // `round(x)` and `round(x, n)` over anything that is not minor
                // units are SQLite's own round, which already agrees with
                // PostgreSQL on a float.
                let [value, places] = exprs.as_slice() else {
                    return Ok(simple_function_expr(
                        "round",
                        exprs
                            .iter()
                            .map(|arg| arg.translate(schema, options))
                            .collect::<Result<Vec<_>, _>>()?,
                        translate_window_type(func.over.as_ref(), schema, options)?,
                    ));
                };
                let Some(scale) = numeric_scale(value, schema) else {
                    return Ok(simple_function_expr(
                        "round",
                        vec![value.translate(schema, options)?, places.translate(schema, options)?],
                        translate_window_type(func.over.as_ref(), schema, options)?,
                    ));
                };
                let Some(target) = literal_integer(places).and_then(|n| u32::try_from(n).ok())
                else {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                        "round({value}, {places}) over a NUMERIC needs the number of places as a                          literal, since the value is held as minor units and the rounding is                          integer arithmetic decided at translation time."
                    )));
                };
                // Down to the requested places and back, so the result keeps
                // the column's scale exactly as PostgreSQL keeps the numeric's.
                let translated = value.translate(schema, options)?;
                let rounded = rescale_minor_units(translated, scale, target.min(scale));
                Ok(rescale_minor_units(rounded, target.min(scale), scale))
            }
            FunctionTranslation::StringAgg => {
                let mut args =
                    translate_function_arguments::<Forward>(&func.args, schema, options)?;
                if let FunctionArguments::List(list) = &mut args
                    && list.duplicate_treatment == Some(DuplicateTreatment::Distinct)
                    && list.args.len() == 2
                {
                    // SQLite answers `DISTINCT aggregates must have exactly one
                    // argument`, in every version, so the separator has to go.
                    // The one group_concat then uses is a comma, which is the
                    // separator nearly every caller passes, and any other has
                    // no faithful form: replacing commas in the joined result
                    // would corrupt any value that contains one.
                    let comma_separated = matches!(
                        list.args.last(),
                        Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                            ValueWithSpan { value: Value::SingleQuotedString(separator), .. },
                        )))) if separator == ","
                    );
                    if !comma_separated {
                        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                            "string_agg(DISTINCT x, sep) has no SQLite form unless the separator \
                             is a comma: group_concat takes no separator argument beside \
                             DISTINCT, and it joins with a comma. Drop the DISTINCT and \
                             de-duplicate in a subquery, or use a comma."
                                .to_string(),
                        ));
                    }
                    list.args.truncate(1);
                }

                Ok(Expr::Function(Function {
                    name: ObjectName::from(vec![Ident::new("group_concat")]),
                    parameters: translate_function_arguments::<Forward>(
                        &func.parameters,
                        schema,
                        options,
                    )?,
                    args,
                    over: translate_window_type(func.over.as_ref(), schema, options)?,
                    filter: None,
                    ..func
                }))
            }
            FunctionTranslation::CharLength { label } => {
                let exprs = extract_exactly(&func.args, 1, label)?;
                let argument = exprs[0];
                // PostgreSQL has no char_length over anything but text:
                // `char_length(u)` on a uuid column answers `function
                // char_length(uuid) does not exist`. SQLite's length takes the
                // column anyway and counts a blob's bytes, so a UUID stored as
                // one answered 16 for a query PostgreSQL never runs.
                if resolves_to_non_textual_column(argument, schema) {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                        "{label}({argument}) has no PostgreSQL meaning: {label} is defined over \
                         text, and this column is not. PostgreSQL answers `function {label}(...) \
                         does not exist`. Use length() for a binary column, or cast the operand \
                         to text."
                    )));
                }
                Ok(simple_function_expr(
                    "length",
                    vec![argument.translate(schema, options)?],
                    translate_window_type(func.over.as_ref(), schema, options)?,
                ))
            }
            FunctionTranslation::Quote { nullable } => {
                let label = if nullable { "quote_nullable" } else { "quote_literal" };
                let exprs = extract_exactly(&func.args, 1, label)?;
                // PostgreSQL casts to text before quoting, so `quote_literal(42)`
                // is the four characters `'42'`. SQLite's `quote` renders a
                // number as a bare numeric literal instead, which is different
                // SQL from a function whose whole purpose is building SQL.
                let quoted = simple_function_expr(
                    "quote",
                    vec![Expr::Cast {
                        expr: Box::new(exprs[0].translate(schema, options)?),
                        data_type: DataType::Text,
                        format: None,
                        kind: CastKind::Cast,
                    }],
                    None,
                );
                if nullable {
                    return Ok(quoted);
                }
                // `quote` answers the bare word NULL for a NULL argument, which
                // is what quote_nullable wants and quote_literal does not. Any
                // other argument comes back wrapped in apostrophes, so the
                // string `NULL` quotes to a six character `'NULL'` and this
                // comparison cannot mistake it for the absent value.
                Ok(simple_function_expr("NULLIF", vec![quoted, string_literal("NULL")], None))
            }
            FunctionTranslation::JsonTypeof => {
                let exprs = extract_exactly(&func.args, 1, "json_typeof")?;
                let document = exprs[0].translate(schema, options)?;
                Ok(postgres_json_type_name(simple_function_expr("json_type", vec![document], None)))
            }
            FunctionTranslation::JsonAgg => {
                let exprs = function_argument_exprs(&func.args);
                let [argument] = exprs.as_slice() else {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                        "json_agg takes exactly one argument".to_string(),
                    ));
                };
                let element = argument.translate(schema, options)?;
                // A JSON column is TEXT here, so json_group_array would quote
                // the document into a string. Reading it back with json() is
                // only safe for a column declared JSON: json('hello') is
                // `malformed JSON`.
                let element = if carries_json(argument, schema) {
                    simple_function_expr("json", vec![element], None)
                } else {
                    element
                };

                let mut args =
                    translate_function_arguments::<Forward>(&func.args, schema, options)?;
                replace_first_argument(&mut args, element);
                let aggregate = Expr::Function(Function {
                    name: ObjectName::from(vec![Ident::new("json_group_array")]),
                    parameters: translate_function_arguments::<Forward>(
                        &func.parameters,
                        schema,
                        options,
                    )?,
                    args,
                    over: translate_window_type(func.over.as_ref(), schema, options)?,
                    filter: None,
                    ..func
                });

                // PostgreSQL answers NULL over no rows where json_group_array
                // answers an empty array. An aggregate over one row or more
                // always has an element, so `[]` can only mean no rows.
                Ok(simple_function_expr("NULLIF", vec![aggregate, string_literal("[]")], None))
            }
            FunctionTranslation::ToCbrt => {
                // cbrt(x) -> pow(x, (1.0 / 3.0))
                let exprs = extract_exactly(&func.args, 1, "cbrt")?;
                let x = exprs[0].translate(schema, options)?;
                let exponent = Expr::Nested(Box::new(Expr::BinaryOp {
                    left: Box::new(number_literal("1.0")),
                    op: BinaryOperator::Divide,
                    right: Box::new(number_literal("3.0")),
                }));
                Ok(simple_function_expr("pow", vec![x, exponent], None))
            }
            FunctionTranslation::CovarPop => {
                let (x, y) = two_aggregate_args(&func.args, schema, options, "covar_pop")?;
                Ok(covar_pop_closed_form(&x, &y))
            }
            FunctionTranslation::CovarSamp => {
                let (x, y) = two_aggregate_args(&func.args, schema, options, "covar_samp")?;
                Ok(covar_samp_closed_form(&x, &y))
            }
            FunctionTranslation::Corr => {
                let (x, y) = two_aggregate_args(&func.args, schema, options, "corr")?;
                Ok(corr_closed_form(&x, &y))
            }
            FunctionTranslation::Array(kind) => {
                array::translate_array_function(kind, &func.args, schema, options)
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

#[cfg(all(test, feature = "std"))]
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
