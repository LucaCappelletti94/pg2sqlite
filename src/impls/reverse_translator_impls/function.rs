//! Implementation of the [`crate::traits::ReverseTranslator`] trait for the
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
    BinaryOperator, CastKind, DataType, DateTimeField, Expr, ExtractSyntax, Function, FunctionArg,
    FunctionArgExpr, FunctionArguments, Ident, ObjectName, ObjectNamePart, TrimWhereField, Value,
    ValueWithSpan,
};

use super::helpers::{Reverse, reverse_translate_window_type};
use crate::{
    errors::Error,
    impls::{
        datetime_helpers::datetime_field_from_strftime_format,
        function_helpers::{
            extract_exactly, function_arg_expr_or_err, integer_literal, simple_function_expr,
            string_literal,
        },
        shared_helpers::{
            function_argument_exprs, translate_function_arguments, translate_order_by_expr,
        },
        timezone::{
            TimestampAwareness, flipped_shifting_offset, normalize_timezone_modifier_for_postgres,
            timestamp_awareness,
        },
        translator_impls::expr::sqlite_json_path_to_pg_text_path,
    },
    prelude::Pg2SqliteOptions,
    traits::TranslationOptions,
};

/// Simple reverse renames: `(sqlite_name, pg_name)`.
/// Checked before the main match for a compact fast path.
const REVERSE_RENAMES: &[(&str, &str)] = &[
    ("group_concat", "string_agg"),
    ("json_group_array", "json_agg"),
    ("json_group_object", "json_object_agg"),
    ("unicode", "ascii"),
    ("json_object", "json_build_object"),
    ("json_array", "json_build_array"),
    ("json_type", "json_typeof"),
    ("json_array_length", "jsonb_array_length"),
    ("sqlite_version", "version"),
    ("quote", "quote_nullable"),
    ("json_quote", "to_jsonb"),
    ("ifnull", "COALESCE"),
];

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
    /// Transform vec_f16 to halfvec cast
    ToHalfvecCast,
    /// Transform char to chr
    ToChr,
    /// Transform LTRIM/RTRIM/TRIM(str, chars) to TRIM(LEADING|TRAILING|BOTH
    /// chars FROM str)
    ToTrimDirectional(TrimWhereField),
    /// Transform datetime(epoch, 'unixepoch') to to_timestamp(epoch)
    ToTimestampFromEpoch,
    /// Transform strftime(composite_fmt, ts) to date_trunc(field, ts)
    ToDateTrunc(String),
    /// Transform json(x) to CAST(x AS JSONB)
    ToCastAsJsonb,
    /// Transform json_set/json_insert to their PG equivalents with path
    /// conversion.
    ///
    /// The string names the PostgreSQL target function (`jsonb_set` or
    /// `jsonb_insert`).
    ToJsonbPathFunc(String),
    /// Transform json_remove(j, '$.path') to j #- '{path}'
    ToJsonPathRemove,
    /// Transform json_extract(j, '$.path') to j #> '{path}'
    ToJsonPathExtract,
    /// Transform json_valid(x) to x IS JSON
    ToIsJson,
    /// Transform json_patch(a, b) to a || b (jsonb concatenation)
    ToJsonbConcat,
    /// No translation needed
    PassThrough,
    /// Translate iif(cond, then, else) to CASE WHEN cond THEN then ELSE else
    /// END.
    ToIif,
    /// Translate total(x) to COALESCE(SUM(x), 0).
    ///
    /// SQLite's total always returns 0 for no rows; SUM returns NULL, so the
    /// COALESCE is required for a faithful round-trip.
    ToTotal,
    /// Translate hex(x) to encode(x, 'hex').
    ToEncodeHex,
    /// Translate unhex(x) to decode(x, 'hex').
    ToDecodeHex,
    /// Translate unixepoch(x) to EXTRACT(EPOCH FROM x).
    ToExtractEpoch,
    /// Reject the named SQLite-only construct with the reason string.
    Reject(String),
}

/// Reverse a composite strftime format string back to a `date_trunc` field
/// name. Returns `None` if the format doesn't match a known `date_trunc`
/// pattern.
///
/// The three coarse formats carry ` 00:00:00` because PostgreSQL's
/// `date_trunc` always answers a full timestamp. A date-only format such as
/// `%Y-%m-%d` is deliberately absent: it is a valid thing to write in SQLite,
/// but reversing it to `date_trunc('day', ...)` would hand back PostgreSQL
/// that answers a timestamp where the SQLite it came from answered a date.
fn reverse_strftime_to_date_trunc_field(format: &str) -> Option<&'static str> {
    match format {
        "%Y-01-01 00:00:00" => Some("year"),
        "%Y-%m-01 00:00:00" => Some("month"),
        "%Y-%m-%d 00:00:00" => Some("day"),
        "%Y-%m-%d %H:00:00" => Some("hour"),
        "%Y-%m-%d %H:%M:00" => Some("minute"),
        "%Y-%m-%d %H:%M:%S" => Some("second"),
        _ => None,
    }
}

/// The PostgreSQL zone for a SQLite `datetime` timezone modifier.
///
/// The forward direction flips an aware operand's offset, because a bare
/// timestamp and a timestamptz shift opposite ways, so this has to flip it
/// back. Over `2023-01-15 12:00:00` PostgreSQL answers 17:30 for the naive
/// operand and 06:30 for the aware one, both measured on 16, and handing the
/// stored sign straight back turns the second into the first.
///
/// An operand not known to be a timestamp is refused, for either of two
/// reasons, and the message says which. A shifting offset cannot pick its sign
/// without the type. Every other spelling can, but PostgreSQL applies `AT TIME
/// ZONE` only to a timestamp, so emitting one over a text column answered
/// `function pg_catalog.timezone(unknown, text) does not exist`.
///
/// The forward direction deliberately does NOT require the type for the second
/// reason, and the asymmetry is the two type systems rather than an oversight:
/// SQLite is dynamically typed, so `datetime(txt, '+05:30')` over a text column
/// answers 17:30 rather than complaining. Both measured.
fn at_time_zone_for_modifier(
    modifier: String,
    timestamp: &Expr,
    schema: &ParserDB,
) -> Result<String, Error> {
    let flipped = flipped_shifting_offset(&modifier);

    let Some(awareness) = timestamp_awareness(timestamp, schema) else {
        return Err(Error::UnsupportedSQLiteFeature(if flipped.is_some() {
            format!(
                "a datetime offset modifier of '{modifier}' shifts a bare timestamp and a \
                 timestamptz in opposite directions, and `{timestamp}` is not known to be \
                 either, so either sign would be wrong half the time. Cast the operand, as \
                 `{timestamp}::timestamptz`, to say which it is."
            )
        } else {
            format!(
                "PostgreSQL applies AT TIME ZONE only to a timestamp, and `{timestamp}` is not \
                 known to be a timestamp here, so reversing the datetime modifier '{modifier}' \
                 onto it would emit SQL PostgreSQL refuses. Cast the operand, as \
                 `{timestamp}::timestamp`, to say what it holds."
            )
        }));
    };

    match (flipped, awareness) {
        (Some(flipped), TimestampAwareness::Aware) => Ok(flipped),
        _ => Ok(modifier),
    }
}

#[allow(clippy::too_many_lines)]
pub fn reverse_function(
    name: &ObjectName,
    args: &FunctionArguments,
    options: &Pg2SqliteOptions,
) -> FunctionReversal {
    let func_name = name.0.last().and_then(|part| part.as_ident()).map_or_else(
        || name.to_string().to_ascii_lowercase(),
        |ident| ident.value.to_ascii_lowercase(),
    );

    // Fast path: check static reverse rename table.
    if let Some(&(_, target)) =
        REVERSE_RENAMES.iter().find(|&&(sqlite, _)| sqlite == func_name.as_str())
    {
        return FunctionReversal::Rename(target.to_string());
    }

    match func_name.as_str() {
        // datetime('now') -> NOW(), datetime(x) -> x AT TIME ZONE 'UTC'
        "datetime" => {
            if let FunctionArguments::List(list) = args
                && list.args.len() == 1
            {
                if let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    ValueWithSpan { value: Value::SingleQuotedString(s), .. },
                )))) = list.args.first()
                    && s == "now"
                {
                    return FunctionReversal::ToNow;
                }
                // The one-argument form is what `AT TIME ZONE 'UTC'` becomes on
                // the way out, since SQLite's own `utc` modifier shifts by the
                // machine's offset and PostgreSQL's UTC shifts nothing.
                return FunctionReversal::ToAtTimeZone("UTC".to_string());
            }
            if let FunctionArguments::List(list) = args
                && list.args.len() == 2
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    ValueWithSpan { value: Value::SingleQuotedString(modifier), .. },
                )))) = list.args.get(1)
            {
                if let Some(zone) = normalize_timezone_modifier_for_postgres(modifier) {
                    return FunctionReversal::ToAtTimeZone(zone);
                }
                if modifier == "unixepoch" {
                    return FunctionReversal::ToTimestampFromEpoch;
                }
            }
            FunctionReversal::PassThrough
        }
        // strftime('%Y-01-01 00:00:00', expr) -> date_trunc('year', expr)
        // strftime('%Y', expr) -> EXTRACT(YEAR FROM expr)
        "strftime" => {
            if let FunctionArguments::List(list) = args
                && list.args.len() == 2
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    ValueWithSpan { value: Value::SingleQuotedString(format), .. },
                )))) = list.args.first()
            {
                if let Some(field) = reverse_strftime_to_date_trunc_field(format) {
                    return FunctionReversal::ToDateTrunc(field.to_string());
                }
                if let Some(field) = datetime_field_from_strftime_format(format) {
                    return FunctionReversal::ToExtract(field);
                }
            }
            FunctionReversal::PassThrough
        }
        // INSTR(str, substr) -> POSITION(substr IN str)
        "instr" => FunctionReversal::ToPosition,
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
        // Single-arg TRIM(str) is parsed as Expr::Trim, not Function; pass through.
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
        // vec_f16(expr) -> expr::halfvec
        "vec_f16" => FunctionReversal::ToHalfvecCast,
        // uuid() / custom UUID function -> gen_random_uuid()
        name if name == options.get_uuid_function_name() => {
            FunctionReversal::Rename("gen_random_uuid".to_string())
        }

        // json(x) -> CAST(x AS JSONB)
        "json" => FunctionReversal::ToCastAsJsonb,
        // json_set(j, '$.path', v) -> jsonb_set(j, '{path}', v)
        "json_set" => FunctionReversal::ToJsonbPathFunc("jsonb_set".to_string()),
        // json_insert(j, '$.path', v) -> jsonb_insert(j, '{path}', v)
        "json_insert" => FunctionReversal::ToJsonbPathFunc("jsonb_insert".to_string()),
        // json_remove(j, '$.path') -> j #- '{path}'
        "json_remove" => FunctionReversal::ToJsonPathRemove,
        // json_extract(j, '$.path') -> j #> '{path}'
        "json_extract" => FunctionReversal::ToJsonPathExtract,
        // json_valid(x) -> x IS JSON
        "json_valid" => FunctionReversal::ToIsJson,
        // json_patch(a, b) -> a || b
        "json_patch" => FunctionReversal::ToJsonbConcat,

        // iif(c, t, f) -> CASE WHEN c THEN t ELSE f END
        "iif" => FunctionReversal::ToIif,
        // total(x) -> COALESCE(SUM(x), 0)
        "total" => FunctionReversal::ToTotal,
        // hex(x) -> encode(x, 'hex')
        "hex" => FunctionReversal::ToEncodeHex,
        // unhex(x) -> decode(x, 'hex')
        "unhex" => FunctionReversal::ToDecodeHex,
        // unixepoch(x) -> EXTRACT(EPOCH FROM x)
        "unixepoch" => FunctionReversal::ToExtractEpoch,
        // typeof(x): pg_typeof returns a registered type name; SQLite returns a
        // storage-class string. The values differ, so this is not a translation.
        "typeof" => {
            FunctionReversal::Reject(
                "typeof: PostgreSQL's pg_typeof returns a registered type name, \
             not a storage-class string, so there is no faithful translation"
                    .to_string(),
            )
        }
        // printf(fmt, ...): SQLite uses C-style format specifiers; PostgreSQL's
        // format uses %s, %I, %L which are incompatible.
        "printf" => {
            FunctionReversal::Reject(
                "printf: SQLite uses C-style format specifiers incompatible with \
             PostgreSQL format's %s, %I, %L specifiers"
                    .to_string(),
            )
        }
        // randomblob(n): needs pgcrypto's gen_random_bytes, an extension.
        "randomblob" => {
            FunctionReversal::Reject(
                "randomblob: requires the pgcrypto extension's gen_random_bytes, \
             which may not be installed"
                    .to_string(),
            )
        }
        // changes(): connection-scoped SQLite state with no PostgreSQL equivalent.
        "changes" => {
            FunctionReversal::Reject(
                "changes: connection-scoped SQLite state with no PostgreSQL equivalent".to_string(),
            )
        }
        // last_insert_rowid(): connection-scoped SQLite state with no PostgreSQL equivalent.
        "last_insert_rowid" => {
            FunctionReversal::Reject(
                "last_insert_rowid: connection-scoped SQLite state with no PostgreSQL equivalent"
                    .to_string(),
            )
        }
        // julianday(x): no PostgreSQL equivalent.
        "julianday" => {
            FunctionReversal::Reject(
                "julianday: no PostgreSQL equivalent; use date arithmetic or EXTRACT instead"
                    .to_string(),
            )
        }
        // random(): SQLite returns a signed 64-bit integer; PostgreSQL returns a double
        // in [0, 1). Passing through silently changes the value range.
        // The forward translator emits (CAST(random() AS REAL) + 9223372036854775808.0)
        // / 18446744073709551616.0, which the expression reverse translator recognises
        // and converts back to random().
        "random" => {
            FunctionReversal::Reject(
                "random: SQLite returns a signed 64-bit integer; PostgreSQL returns a \
             double in [0, 1); passing through silently changes the value range"
                    .to_string(),
            )
        }

        _ => FunctionReversal::PassThrough,
    }
}

/// Build a reverse-translated function: translate args, params, window, filter
/// and within_group, then wrap in `Expr::Function` with the given name.
fn build_reverse_function(
    name: ObjectName,
    func: &Function,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    Ok(Expr::Function(Function {
        name,
        uses_odbc_syntax: func.uses_odbc_syntax,
        parameters: translate_function_arguments::<Reverse>(&func.parameters, schema, options)?,
        args: translate_function_arguments::<Reverse>(&func.args, schema, options)?,
        filter: func
            .filter
            .as_ref()
            .map(|f| {
                crate::prelude::ReverseTranslator::reverse_translate(f.as_ref(), schema, options)
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

#[allow(clippy::too_many_lines)]
pub fn reverse_translate_function(
    func: &Function,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    match reverse_function(&func.name, &func.args, options) {
        FunctionReversal::Rename(new_name) => {
            build_reverse_function(
                ObjectName::from(vec![Ident::new(new_name)]),
                func,
                schema,
                options,
            )
        }
        FunctionReversal::ToNow => {
            // datetime('now') -> NOW()
            Ok(simple_function_expr("NOW", vec![], None))
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
            debug_assert!(
                (1..=2).contains(&list.args.len()),
                "reverse_function classified datetime as ToAtTimeZone with {} args",
                list.args.len()
            );
            let timestamp_expr = function_arg_expr_or_err(
                list.args
                    .first()
                    .expect("reverse_function must provide datetime timestamp argument"),
            )?;
            let reversed_timestamp = crate::prelude::ReverseTranslator::reverse_translate(
                timestamp_expr,
                schema,
                options,
            )?;

            let zone = at_time_zone_for_modifier(time_zone, &reversed_timestamp, schema)?;
            Ok(Expr::AtTimeZone {
                timestamp: Box::new(reversed_timestamp),
                time_zone: Box::new(Expr::Value(ValueWithSpan {
                    value: Value::SingleQuotedString(zone),
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
                let str_expr = function_arg_expr_or_err(&list.args[0])?;
                let substr_expr = function_arg_expr_or_err(&list.args[1])?;

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
                let left_expr = function_arg_expr_or_err(&list.args[0])?;
                let right_expr = function_arg_expr_or_err(&list.args[1])?;

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
                let expr = function_arg_expr_or_err(&list.args[0])?;
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
                });
            }
            Err(Error::UnsupportedSQLiteFeature("vec_f32 requires exactly 1 argument".to_string()))
        }
        FunctionReversal::ToHalfvecCast => {
            // vec_f16(expr) -> expr::halfvec
            if let FunctionArguments::List(list) = &func.args
                && list.args.len() == 1
            {
                let expr = function_arg_expr_or_err(&list.args[0])?;
                let reversed_expr =
                    crate::prelude::ReverseTranslator::reverse_translate(expr, schema, options)?;

                return Ok(Expr::Cast {
                    expr: Box::new(reversed_expr),
                    data_type: sqlparser::ast::DataType::Custom(
                        ObjectName(vec![ObjectNamePart::Identifier(Ident::new("halfvec"))]),
                        vec![],
                    ),
                    format: None,
                    kind: sqlparser::ast::CastKind::DoubleColon,
                });
            }
            Err(Error::UnsupportedSQLiteFeature("vec_f16 requires exactly 1 argument".to_string()))
        }
        FunctionReversal::ToChr => {
            build_reverse_function(ObjectName::from(vec![Ident::new("chr")]), func, schema, options)
        }
        FunctionReversal::ToTrimDirectional(field) => {
            // LTRIM/RTRIM(str, chars) or TRIM(str, chars)
            // -> TRIM(LEADING|TRAILING|BOTH chars FROM str)
            let FunctionArguments::List(list) = &func.args else {
                unreachable!("reverse_function only returns ToTrimDirectional for List args");
            };
            debug_assert_eq!(list.args.len(), 2, "ToTrimDirectional requires exactly 2 args");
            let str_expr = function_arg_expr_or_err(&list.args[0])?;
            let char_expr = function_arg_expr_or_err(&list.args[1])?;
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
        FunctionReversal::ToTimestampFromEpoch => {
            let FunctionArguments::List(list) = &func.args else {
                return Err(Error::UnsupportedSQLiteFeature(
                    "datetime unixepoch requires list arguments".to_string(),
                ));
            };
            let epoch_expr = function_arg_expr_or_err(
                list.args.first().expect("datetime must have epoch argument"),
            )?;
            let reversed_epoch =
                crate::prelude::ReverseTranslator::reverse_translate(epoch_expr, schema, options)?;
            Ok(simple_function_expr("to_timestamp", vec![reversed_epoch], None))
        }
        FunctionReversal::ToDateTrunc(field) => {
            let FunctionArguments::List(list) = &func.args else {
                return Err(Error::UnsupportedSQLiteFeature(
                    "strftime requires list arguments for date_trunc reversal".to_string(),
                ));
            };
            let ts_expr = function_arg_expr_or_err(
                list.args.get(1).expect("strftime must have timestamp argument"),
            )?;
            let reversed_ts =
                crate::prelude::ReverseTranslator::reverse_translate(ts_expr, schema, options)?;
            let translated_over =
                reverse_translate_window_type(func.over.as_ref(), schema, options)?;
            Ok(simple_function_expr(
                "date_trunc",
                vec![string_literal(&field), reversed_ts],
                translated_over,
            ))
        }
        FunctionReversal::ToCastAsJsonb => {
            // json(x) -> CAST(x AS JSONB)
            let exprs = extract_exactly(&func.args, 1, "json")?;
            let reversed =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            Ok(Expr::Cast {
                expr: Box::new(reversed),
                data_type: DataType::Custom(
                    ObjectName(vec![ObjectNamePart::Identifier(Ident::new("JSONB"))]),
                    vec![],
                ),
                format: None,
                kind: CastKind::Cast,
            })
        }
        FunctionReversal::ToJsonbPathFunc(pg_func) => {
            // json_set(j, '$.a', v, ...) -> jsonb_set(j, '{a}', v)
            // json_insert(j, '$.a', v) -> jsonb_insert(j, '{a}', v)
            let exprs = function_argument_exprs(&func.args);
            if exprs.len() < 3 {
                return Err(Error::UnsupportedSQLiteFeature(format!(
                    "{pg_func} requires at least 3 arguments"
                )));
            }
            let json_expr =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            let path_str = match exprs[1] {
                Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(s), .. }) => {
                    sqlite_json_path_to_pg_text_path(s).ok_or_else(|| {
                        Error::UnsupportedSQLiteFeature(format!(
                            "{pg_func} JSON path must be a simple dotted literal like '$.a.b'; \
                         paths with array indices or non-dot separators cannot be converted"
                        ))
                    })?
                }
                _ => {
                    return Err(Error::UnsupportedSQLiteFeature(format!(
                        "{pg_func} JSON path must be a string literal; \
                         non-literal paths cannot be converted at translation time"
                    )));
                }
            };
            let value_expr =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[2], schema, options)?;
            Ok(simple_function_expr(
                &pg_func,
                vec![json_expr, string_literal(&path_str), value_expr],
                None,
            ))
        }
        FunctionReversal::ToJsonPathRemove => {
            // json_remove(j, '$.a') -> j #- '{a}'
            let exprs = function_argument_exprs(&func.args);
            if exprs.len() < 2 {
                return Err(Error::UnsupportedSQLiteFeature(
                    "json_remove requires at least 2 arguments".to_string(),
                ));
            }
            let json_expr =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            let path_str = match exprs[1] {
                Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(s), .. }) => {
                    sqlite_json_path_to_pg_text_path(s).ok_or_else(|| {
                        Error::UnsupportedSQLiteFeature(
                            "json_remove path must be a simple dotted literal like '$.a'; \
                         paths with array indices cannot be converted"
                                .to_string(),
                        )
                    })?
                }
                _ => {
                    return Err(Error::UnsupportedSQLiteFeature(
                        "json_remove JSON path must be a string literal; \
                         non-literal paths cannot be converted at translation time"
                            .to_string(),
                    ));
                }
            };
            Ok(Expr::BinaryOp {
                left: Box::new(json_expr),
                op: BinaryOperator::HashMinus,
                right: Box::new(string_literal(&path_str)),
            })
        }
        FunctionReversal::ToJsonPathExtract => {
            // json_extract(j, '$.a') -> j #> '{a}'
            let exprs = function_argument_exprs(&func.args);
            if exprs.len() < 2 {
                return Err(Error::UnsupportedSQLiteFeature(
                    "json_extract requires at least 2 arguments".to_string(),
                ));
            }
            let json_expr =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            let path_str = match exprs[1] {
                Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(s), .. }) => {
                    sqlite_json_path_to_pg_text_path(s).ok_or_else(|| {
                        Error::UnsupportedSQLiteFeature(
                            "json_extract path must be a simple dotted literal like '$.a'; \
                         paths with array indices cannot be converted"
                                .to_string(),
                        )
                    })?
                }
                _ => {
                    return Err(Error::UnsupportedSQLiteFeature(
                        "json_extract JSON path must be a string literal; \
                         non-literal paths cannot be converted at translation time"
                            .to_string(),
                    ));
                }
            };
            Ok(Expr::BinaryOp {
                left: Box::new(json_expr),
                op: BinaryOperator::HashArrow,
                right: Box::new(string_literal(&path_str)),
            })
        }
        FunctionReversal::ToIsJson => {
            // json_valid(x) -> x IS JSON
            let exprs = extract_exactly(&func.args, 1, "json_valid")?;
            let reversed =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            Ok(Expr::IsJson {
                expr: Box::new(reversed),
                negated: false,
                kind: None,
                unique_keys: None,
            })
        }
        FunctionReversal::ToJsonbConcat => {
            // json_patch(a, b) -> a || b
            let exprs = extract_exactly(&func.args, 2, "json_patch")?;
            let left =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            let right =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[1], schema, options)?;
            Ok(Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOperator::StringConcat,
                right: Box::new(right),
            })
        }
        FunctionReversal::ToIif => {
            // iif(cond, then, else) -> CASE WHEN cond THEN then ELSE else END
            let exprs = extract_exactly(&func.args, 3, "iif")?;
            let cond =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            let then =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[1], schema, options)?;
            let else_result =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[2], schema, options)?;
            Ok(crate::impls::expr_helpers::case_when(cond, then, Some(else_result)))
        }
        FunctionReversal::ToTotal => {
            // total(x) -> COALESCE(SUM(x), 0)
            let exprs = extract_exactly(&func.args, 1, "total")?;
            let inner =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            let sum = simple_function_expr("SUM", vec![inner], None);
            Ok(simple_function_expr("COALESCE", vec![sum, integer_literal(0)], None))
        }
        FunctionReversal::ToEncodeHex => {
            // hex(x) -> encode(x, 'hex')
            let exprs = extract_exactly(&func.args, 1, "hex")?;
            let inner =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            Ok(simple_function_expr("encode", vec![inner, string_literal("hex")], None))
        }
        FunctionReversal::ToDecodeHex => {
            // unhex(x) -> decode(x, 'hex')
            let exprs = extract_exactly(&func.args, 1, "unhex")?;
            let inner =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            Ok(simple_function_expr("decode", vec![inner, string_literal("hex")], None))
        }
        FunctionReversal::ToExtractEpoch => {
            // unixepoch(x) -> EXTRACT(EPOCH FROM x)
            let exprs = extract_exactly(&func.args, 1, "unixepoch")?;
            let inner =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            Ok(Expr::Extract {
                field: DateTimeField::Epoch,
                syntax: ExtractSyntax::From,
                expr: Box::new(inner),
            })
        }
        FunctionReversal::Reject(msg) => Err(Error::UnsupportedSQLiteFeature(msg)),
        FunctionReversal::PassThrough => {
            build_reverse_function(func.name.clone(), func, schema, options)
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
            ValueWithSpan,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::reverse_translate_function;
    use crate::{
        impls::{function_helpers::function_arg_expr_or_err, timezone::is_fixed_utc_offset},
        prelude::Pg2SqliteOptions,
    };

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
        let err = function_arg_expr_or_err(&wildcard).expect_err("wildcard should be rejected");
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
    fn function_arg_expr_or_err_accepts_expr_named() {
        let arg = FunctionArg::ExprNamed {
            name: parse_expr("param"),
            arg: FunctionArgExpr::Expr(parse_expr("value")),
            operator: FunctionArgOperator::Equals,
        };
        let extracted =
            function_arg_expr_or_err(&arg).expect("expr-named args should be supported");
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
