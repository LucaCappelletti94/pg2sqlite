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
    FunctionArgExpr, FunctionArguments, Ident, ObjectName, ObjectNamePart, TimezoneInfo,
    TrimWhereField, Value, ValueWithSpan,
};

use super::helpers::{Reverse, reverse_translate_window_type};
use crate::{
    errors::Error,
    impls::{
        datetime_helpers::{datetime_field_from_strftime_format, strftime_format_to_pg_to_char},
        function_helpers::{
            extract_exactly, function_arg_expr_or_err, integer_literal, simple_function_expr,
            string_literal,
        },
        idioms::{
            extract_json_group_array_nullif, extract_json_group_object_nullif,
            is_now_localtime_args,
        },
        session_variable,
        shared_helpers::{
            every_declared_type_matches, function_argument_exprs, translate_function_arguments,
            translate_order_by_expr,
        },
        sqlite_functions::classify,
        timezone::{
            TimestampAwareness, flipped_shifting_offset, normalize_timezone_modifier_for_postgres,
            timestamp_awareness,
        },
        translator_impls::{expr::sqlite_json_path_to_pg_text_path, postgis},
    },
    prelude::Pg2SqliteOptions,
    traits::{SessionVariableMapping, TranslationOptions},
};

/// Simple reverse renames: `(sqlite_name, pg_name)`.
/// Checked before the main match for a compact fast path.
const REVERSE_RENAMES: &[(&str, &str)] = &[
    ("json_group_array", "json_agg"),
    ("json_group_object", "json_object_agg"),
    ("json_object", "json_build_object"),
    ("json_array", "json_build_array"),
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
    /// Translate json_type(expr) to json_typeof(expr) or jsonb_typeof(expr)
    /// based on the argument's declared column type. The choice is deferred to
    /// `reverse_translate_function` because that stage has schema access.
    JsonTypeOf,
    /// Translate json_array_length(expr) to json_array_length(expr) or
    /// jsonb_array_length(expr) based on the argument's declared column type.
    /// PostgreSQL has json_array_length(json) and jsonb_array_length(jsonb)
    /// as distinct overloads; SQLite's json_array_length takes any JSON value.
    JsonArrayLength,
    ToEncodeHex,
    /// Translate unhex(x) to decode(x, 'hex').
    ToDecodeHex,
    /// Translate unixepoch(x) to EXTRACT(EPOCH FROM x).
    ToExtractEpoch,
    /// Translate `group_concat(x)` to `string_agg(x, ',')`.
    ///
    /// SQLite's one-argument spelling joins with a comma. PostgreSQL has no
    /// one-argument `string_agg`, so the comma is written out.
    ToStringAgg,
    /// Translate `unicode(x)` to `NULLIF(ascii(x), 0)`: the two agree on
    /// every input except the empty string, where `unicode` answers NULL and
    /// PostgreSQL's `ascii` answers 0, and 0 arises for no other input.
    ToAsciiNullif,
    /// Translate `strftime(fmt, x)` to `to_char(x, template)`, the string
    /// naming the PostgreSQL template.
    ToChar(String),
    /// The function a session variable mapping pairs with a PostgreSQL
    /// setting, which becomes the setting again.
    ToSessionVariable(SessionVariableMapping),
    /// Cast the single argument to the named type, which is how `date(x)` and
    /// `time(x)` cross: PostgreSQL has both names, but `time` is a type name
    /// there and will not take a bare call, and the cast says the same thing
    /// without depending on name resolution at all.
    ToCast(DataType),
    /// Emit a bare keyword with no argument list, which is the only form
    /// PostgreSQL takes for `current_date` and its relatives.
    ToBareKeyword(&'static str),
    /// Reject the named SQLite-only construct with the reason string.
    Reject(String),
}

/// What `date(...)` and `time(...)` become, which their arity decides.
///
/// SQLite answers the current date or time for the argument-less spelling,
/// measured on 3.51.1, so that becomes the keyword. One argument is the date or
/// time part of a timestamp, which is what the cast says. A second argument is
/// a modifier, `'+1 day'` or `'start of month'`, and PostgreSQL expresses those
/// as interval arithmetic rather than as arguments, so they are refused.
fn reverse_date_or_time(
    args: &FunctionArguments,
    data_type: DataType,
    keyword: &'static str,
    label: &str,
) -> FunctionReversal {
    match args {
        FunctionArguments::List(list) if list.args.is_empty() => {
            FunctionReversal::ToBareKeyword(keyword)
        }
        FunctionArguments::None => FunctionReversal::ToBareKeyword(keyword),
        FunctionArguments::List(list) if list.args.len() == 1 => {
            FunctionReversal::ToCast(data_type)
        }
        _ => {
            FunctionReversal::Reject(format!(
                "{label} with a modifier argument has no PostgreSQL form: PostgreSQL shifts a \
                 timestamp with interval arithmetic, as `x + interval '1 day'`, or truncates it \
                 with date_trunc, rather than taking modifiers. The single-argument \
                 {label}(x) reverses, as x::{label}."
            ))
        }
    }
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

/// What a `strftime` call becomes, or why it cannot become anything.
///
/// PostgreSQL has no `strftime`, so a format outside the three tables, a
/// format that is not a literal, and the extra modifier arguments SQLite
/// allows after the timestamp all have to be refused rather than forwarded.
fn reverse_strftime(args: &FunctionArguments) -> FunctionReversal {
    let FunctionArguments::List(list) = args else {
        return FunctionReversal::Reject(strftime_rejection("a call with no argument list"));
    };
    if list.args.len() != 2 {
        return FunctionReversal::Reject(strftime_rejection(
            "a call with anything but a format and a timestamp, since SQLite's trailing date \
             modifiers have no PostgreSQL form",
        ));
    }
    let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(ValueWithSpan {
        value: Value::SingleQuotedString(format),
        ..
    })))) = list.args.first()
    else {
        return FunctionReversal::Reject(strftime_rejection(
            "a format that is not a string literal, which cannot be read at translation time",
        ));
    };

    if let Some(field) = reverse_strftime_to_date_trunc_field(format) {
        return FunctionReversal::ToDateTrunc(field.to_string());
    }
    if let Some(field) = datetime_field_from_strftime_format(format) {
        return FunctionReversal::ToExtract(field);
    }
    if let Some(template) = strftime_format_to_pg_to_char(format) {
        return FunctionReversal::ToChar(template);
    }
    FunctionReversal::Reject(strftime_rejection(&format!("the format '{format}'")))
}

/// The refusal every unreversible `strftime` shares, which names what does
/// reverse so the author can pick one.
fn strftime_rejection(subject: &str) -> String {
    format!(
        "strftime has no PostgreSQL equivalent, and this is {subject}. These reverse: the six \
         truncating formats ('%Y-01-01 00:00:00', '%Y-%m-01 00:00:00', '%Y-%m-%d 00:00:00', \
         '%Y-%m-%d %H:00:00', '%Y-%m-%d %H:%M:00', '%Y-%m-%d %H:%M:%S') become date_trunc, a \
         single field (%Y %m %d %H %M %S %f %s %V %G %u %w %j) becomes EXTRACT, and a format \
         built from %Y %G %V %u %m %d %H %I %M %S with separators becomes to_char."
    )
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
        "group_concat" => FunctionReversal::ToStringAgg,
        // json_array_length(json) exists in PostgreSQL, but jsonb_array_length(jsonb) is the
        // overload for JSONB-typed arguments. The choice depends on the argument's declared
        // column type and is resolved in reverse_translate_function where schema is available.
        "json_array_length" => FunctionReversal::JsonArrayLength,
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
            // localtimestamp: datetime('now', 'localtime') -> LOCALTIMESTAMP.
            // This must be checked before the generic 2-arg timezone path, which
            // would map 'localtime' to AT TIME ZONE and then fail because 'now'
            // is not a known timestamp expression.
            if is_now_localtime_args(args) {
                return FunctionReversal::ToBareKeyword("LOCALTIMESTAMP");
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
            // `datetime(e, 'unixepoch', 'subsec')` is the same conversion with
            // the fraction kept, which `to_timestamp` carries anyway.
            if let FunctionArguments::List(list) = args
                && list.args.len() == 3
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    ValueWithSpan { value: Value::SingleQuotedString(epoch), .. },
                )))) = list.args.get(1)
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    ValueWithSpan { value: Value::SingleQuotedString(subsec), .. },
                )))) = list.args.get(2)
                && epoch == "unixepoch"
                && subsec == "subsec"
            {
                return FunctionReversal::ToTimestampFromEpoch;
            }
            // Anything else has no PostgreSQL spelling: PostgreSQL has no
            // datetime function, so a call that is neither a zone shift nor
            // an epoch conversion cannot be emitted. The refusal names the
            // modifier when there is exactly one, and stays generic for a
            // chain, whose first modifier alone would mislead (R98).
            let sole_modifier = if let FunctionArguments::List(list) = args
                && list.args.len() == 2
                && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                    ValueWithSpan { value: Value::SingleQuotedString(modifier), .. },
                )))) = list.args.get(1)
            {
                Some(modifier.clone())
            } else {
                None
            };
            FunctionReversal::Reject(match sole_modifier {
                Some(modifier) => {
                    format!(
                        "the datetime modifier '{modifier}' has no PostgreSQL form. PostgreSQL has \
                     no datetime function, and only a time zone modifier or unixepoch can be \
                     rewritten onto AT TIME ZONE or to_timestamp. Express the arithmetic with \
                     interval arithmetic or date_trunc before translating."
                    )
                }
                None => {
                    "this datetime call has no PostgreSQL form. PostgreSQL has no datetime \
                         function, and only datetime('now'), a single timestamp argument, one \
                         time zone modifier, or unixepoch can be rewritten onto NOW(), AT TIME \
                         ZONE, or to_timestamp."
                        .to_string()
                }
            })
        }
        // date(x) -> x::date, time(x) -> x::time, and the argument-less
        // spellings -> the keywords, which is what SQLite answers for them.
        // PostgreSQL has a `date` function and a `time` one, but `time` is a
        // type name there and refuses a bare call, so the cast is what crosses
        // for both. A modifier argument has no counterpart at all.
        "date" => reverse_date_or_time(args, DataType::Date, "current_date", "date"),
        "time" => {
            // localtime: time('now', 'localtime') -> LOCALTIME.
            if is_now_localtime_args(args) {
                return FunctionReversal::ToBareKeyword("LOCALTIME");
            }
            reverse_date_or_time(
                args,
                DataType::Time(None, TimezoneInfo::None),
                "current_time",
                "time",
            )
        }
        // strftime('%Y-01-01 00:00:00', expr) -> date_trunc('year', expr)
        // strftime('%Y', expr) -> EXTRACT(YEAR FROM expr)
        // strftime('%Y-%m-%d', expr) -> to_char(expr, 'YYYY-MM-DD')
        "strftime" => reverse_strftime(args),
        // INSTR(str, substr) -> POSITION(substr IN str)
        "instr" => FunctionReversal::ToPosition,
        // unicode(x) -> NULLIF(ascii(x), 0), NULL for the empty string.
        "unicode" => FunctionReversal::ToAsciiNullif,
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
        // The caller's identity, which a mapping pairs with this name. Checked
        // before the UUID options, and like them ahead of the catch-all,
        // because a pairing is the caller stating what the name means here.
        name if session_variable::mapping_for_function(name, options).is_some() => {
            let mapping = session_variable::mapping_for_function(name, options)
                .expect("the guard just found the mapping");
            if session_variable::call_has_no_arguments(args) {
                FunctionReversal::ToSessionVariable(mapping.clone())
            } else {
                FunctionReversal::Reject(session_variable::paired_arity_refusal(mapping))
            }
        }
        // The declared version 7 generator -> uuidv7(). Checked before the
        // random one, so a caller who pointed both options at one name gets
        // the reading that keeps the creation-time ordering.
        name if options.get_uuid_v7_function_name() == Some(name) => {
            FunctionReversal::Rename("uuidv7".to_string())
        }
        // uuid() / custom UUID function -> gen_random_uuid()
        name if name == options.get_uuid_function_name() => {
            FunctionReversal::Rename("gen_random_uuid".to_string())
        }
        // json(x) -> CAST(x AS JSONB)
        "json" => FunctionReversal::ToCastAsJsonb,
        // json_set(j, '$.path', v) -> jsonb_set(j, '{path}', to_jsonb(v))
        "json_set" => FunctionReversal::ToJsonbPathFunc("jsonb_set".to_string()),
        // json_insert(j, '$.path', v) -> jsonb_insert(j, '{path}', to_jsonb(v))
        "json_insert" => FunctionReversal::ToJsonbPathFunc("jsonb_insert".to_string()),
        // json_remove(j, '$.path') -> j #- '{path}'
        "json_remove" => FunctionReversal::ToJsonPathRemove,
        // json_extract(j, '$.path') -> j #> '{path}'
        "json_extract" => FunctionReversal::ToJsonPathExtract,
        // json_valid(x) -> x IS JSON
        "json_valid" => FunctionReversal::ToIsJson,
        // json_patch(a, b) -> a || b
        "json_patch" => FunctionReversal::ToJsonbConcat,
        // json_type(x): use jsonb_typeof when the argument's declared type is
        // JSONB, json_typeof otherwise. The choice is resolved in
        // reverse_translate_function using the schema.
        "json_type" => FunctionReversal::JsonTypeOf,
        // iif(c, t, f) -> CASE WHEN c THEN t ELSE f END
        "iif" => FunctionReversal::ToIif,
        // total(x) -> COALESCE(SUM(x), 0)
        "total" => FunctionReversal::ToTotal,
        // hex(x) -> encode(x::bytea, 'hex')
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

        name => classify_unreversed(name, args, options),
    }
}

/// SQLite names PostgreSQL cannot answer, each with what it would take instead.
///
/// Sorted, and searched rather than matched, because the list is data: it is
/// the complement of [`SHARED_WITH_POSTGRES`](crate::impls::sqlite_functions)
/// over SQLite's own inventory, and both were measured against PostgreSQL 17's
/// catalogue.
const SQLITE_ONLY: &[(&str, &str)] = &[
    (
        "format",
        "SQLite's format is printf, whose C-style specifiers PostgreSQL's format does not \
     share: it templates with %s, %I and %L",
    ),
    (
        "glob",
        "glob is SQLite's case-sensitive shell-style match, which PostgreSQL has no function \
     for: write LIKE or a regular expression",
    ),
    ("if", "if is not a PostgreSQL function: write CASE WHEN, or iif, which does reverse"),
    (
        "json_each",
        "SQLite's json_each answers a row per element with key, value, type, atom, id, \
     parent, fullkey and path, where PostgreSQL's answers key and value alone and refuses an array \
     outright, so the two are not the same table. PostgreSQL splits the job: json_array_elements \
     walks an array, json_each an object",
    ),
    (
        "json_error_position",
        "json_error_position reports where SQLite's parser gave up, and \
     PostgreSQL has nothing that reports it",
    ),
    (
        "json_pretty",
        "json_pretty has no PostgreSQL equivalent taking the same argument: \
     jsonb_pretty takes jsonb, so the value has to be cast first",
    ),
    (
        "json_replace",
        "json_replace updates only paths that already exist, and jsonb_set with \
     create_if_missing false is the PostgreSQL shape, which takes a text[] path rather than a \
     JSONPath string",
    ),
    (
        "json_tree",
        "SQLite's json_tree walks a document recursively, and PostgreSQL has no \
     function that answers the same rows",
    ),
    (
        "jsonb",
        "SQLite's jsonb functions answer its own binary encoding, which is not \
     PostgreSQL's: the json spellings are what reverse",
    ),
    (
        "jsonb_array",
        "SQLite's jsonb functions answer its own binary encoding, which is not \
     PostgreSQL's: the json spellings are what reverse",
    ),
    (
        "jsonb_extract",
        "SQLite's jsonb functions answer its own binary encoding, which is not \
     PostgreSQL's: the json spellings are what reverse",
    ),
    (
        "jsonb_group_array",
        "SQLite's jsonb functions answer its own binary encoding, which is not \
     PostgreSQL's: the json spellings are what reverse",
    ),
    (
        "jsonb_group_object",
        "SQLite's jsonb functions answer its own binary encoding, which is not \
     PostgreSQL's: the json spellings are what reverse",
    ),
    (
        "jsonb_insert",
        "SQLite's jsonb functions answer its own binary encoding, and PostgreSQL's \
     jsonb_insert takes a text[] path rather than a JSONPath string: the json spellings are what \
     reverse",
    ),
    (
        "jsonb_object",
        "SQLite's jsonb functions answer its own binary encoding, and PostgreSQL's \
     jsonb_object builds from arrays rather than from key and value pairs: the json spellings are \
     what reverse",
    ),
    (
        "jsonb_patch",
        "SQLite's jsonb functions answer its own binary encoding, which is not \
     PostgreSQL's: the json spellings are what reverse",
    ),
    (
        "jsonb_remove",
        "SQLite's jsonb functions answer its own binary encoding, which is not \
     PostgreSQL's: the json spellings are what reverse",
    ),
    (
        "jsonb_replace",
        "SQLite's jsonb functions answer its own binary encoding, which is not \
     PostgreSQL's: the json spellings are what reverse",
    ),
    (
        "jsonb_set",
        "SQLite's jsonb functions answer its own binary encoding, and PostgreSQL's \
     jsonb_set takes a text[] path rather than a JSONPath string: the json spellings are what \
     reverse",
    ),
    (
        "like",
        "like is a reserved word in PostgreSQL, which refuses it as a bare call: write the \
     LIKE operator",
    ),
    (
        "likelihood",
        "likelihood is a SQLite planner hint with no PostgreSQL equivalent, and it \
     answers its first argument unchanged",
    ),
    (
        "likely",
        "likely is a SQLite planner hint with no PostgreSQL equivalent, and it answers its \
     argument unchanged",
    ),
    (
        "log2",
        "log2 is the one SQLite math function PostgreSQL has no name for, measured against its \
     catalogue: write log(2, x), or ln(x) / ln(2) for a double",
    ),
    (
        "raise",
        "raise is SQLite's trigger abort, which PostgreSQL spells as a PL/pgSQL RAISE \
     statement rather than an expression",
    ),
    ("soundex", "soundex needs PostgreSQL's fuzzystrmatch extension, which may not be installed"),
    (
        "sqlite_source_id",
        "sqlite_source_id names the SQLite build, and PostgreSQL has nothing \
     that identifies it",
    ),
    (
        "timediff",
        "timediff answers SQLite's own duration text, where PostgreSQL subtracts \
     timestamps into an interval",
    ),
    (
        "total_changes",
        "total_changes is connection-scoped SQLite state with no PostgreSQL \
     equivalent",
    ),
    (
        "unlikely",
        "unlikely is a SQLite planner hint with no PostgreSQL equivalent, and it answers \
     its argument unchanged",
    ),
    (
        "zeroblob",
        "zeroblob makes a blob of N zero bytes, which PostgreSQL writes as \
     repeat('\\000', n)::bytea rather than as a function of its own",
    ),
];

/// Classifies a name no arm recognised.
///
/// The default is a refusal, for the reason the forward direction refuses an
/// unrecognised name: emitting it produces SQL that fails at run time, long
/// after translation reported success. Three things earn a passthrough, and all
/// are evidence PostgreSQL has the name: this crate's inventory says the two
/// engines share it, its inventory says PostgreSQL has it while SQLite does
/// not, or the caller declared it.
fn classify_unreversed(
    name: &str,
    args: &FunctionArguments,
    options: &Pg2SqliteOptions,
) -> FunctionReversal {
    let class = classify(name);
    if class.shared_with_postgres
        || class.postgres_only
        || options.declares_user_defined_function(name)
    {
        return FunctionReversal::PassThrough;
    }

    // The geometry option says the deployment has the geometry catalogue, and
    // these names are PostGIS's own, so it says as much about the server as
    // about the replica. The arity is checked because the catalogue records
    // which arities the extension implements.
    if options.is_sqlitegis_enabled()
        && let Some(arity) = positional_arity(args)
        && postgis::is_sqlitegis_function(name, arity)
    {
        return FunctionReversal::PassThrough;
    }

    if let Some(reason) = sqlite_only_reason(name) {
        return FunctionReversal::Reject(reason);
    }

    FunctionReversal::Reject(format!(
        "{name}() is not a name this crate knows PostgreSQL has, and no arm reverses it. Emitting \
         it would produce SQL that fails with `function {name}() does not exist` unless the \
         server answers it. If PostgreSQL has it, declare it with \
         with_user_defined_functions([\"{name}\"])."
    ))
}

/// Why a SQLite-only name cannot cross, when it is one.
///
/// Shared with the row-source position, where `FROM json_each(x)` names the
/// same function in a place the expression classifier never sees.
pub(crate) fn sqlite_only_reason(name: &str) -> Option<String> {
    SQLITE_ONLY
        .binary_search_by_key(&name, |&(sqlite, _)| sqlite)
        .ok()
        .map(|index| format!("{}: {}", SQLITE_ONLY[index].0, SQLITE_ONLY[index].1))
}

/// The positional argument count, or `None` for a shape where arity says
/// nothing.
fn positional_arity(args: &FunctionArguments) -> Option<i32> {
    match args {
        FunctionArguments::List(list) => i32::try_from(list.args.len()).ok(),
        FunctionArguments::None => Some(0),
        FunctionArguments::Subquery(_) => None,
    }
}

/// `value #> '{a,b}'`, PostgreSQL's reading of a SQLite JSON path.
///
/// Shared by `json_extract`, which is the extraction itself, and by the
/// two-argument `json_type`, which asks for the type at a path that PostgreSQL
/// takes outside the call. `caller` names the SQLite function in the refusals,
/// so a path this crate cannot convert says which call it came from.
fn json_path_extraction(
    value: &Expr,
    path: &Expr,
    caller: &str,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    let Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(literal), .. }) = path else {
        return Err(Error::UnsupportedSQLiteFeature(format!(
            "{caller} JSON path must be a string literal; non-literal paths cannot be converted \
             at translation time"
        )));
    };
    let text_path = sqlite_json_path_to_pg_text_path(literal).ok_or_else(|| {
        Error::UnsupportedSQLiteFeature(format!(
            "{caller} path must be a simple dotted literal like '$.a'; paths with array indices \
             cannot be converted"
        ))
    })?;
    let reversed = crate::prelude::ReverseTranslator::reverse_translate(value, schema, options)?;
    Ok(Expr::BinaryOp {
        left: Box::new(reversed),
        op: BinaryOperator::HashArrow,
        right: Box::new(string_literal(&text_path)),
    })
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

/// `floor(extract(epoch from now()))::bigint`, which is what SQLite's
/// `unixepoch()` answers.
///
/// SQLite gives whole seconds as an integer, measured, while
/// `extract(epoch from now())` gives `1786200937.154282`, so neither the floor
/// nor the cast is decoration.
fn current_epoch_seconds() -> Expr {
    let epoch = Expr::Extract {
        field: DateTimeField::Epoch,
        syntax: ExtractSyntax::From,
        expr: Box::new(simple_function_expr("NOW", vec![], None)),
    };
    Expr::Cast {
        expr: Box::new(simple_function_expr("floor", vec![epoch], None)),
        data_type: DataType::BigInt(None),
        format: None,
        kind: CastKind::DoubleColon,
    }
}

/// Writes out the comma SQLite's one-argument `group_concat` joins with, since
/// PostgreSQL's `string_agg` has no one-argument form.
fn with_default_separator(mut reversed: Expr) -> Expr {
    if let Expr::Function(function) = &mut reversed
        && let FunctionArguments::List(list) = &mut function.args
        && list.args.len() == 1
    {
        list.args.push(FunctionArg::Unnamed(FunctionArgExpr::Expr(string_literal(","))));
    }
    reversed
}

#[allow(clippy::too_many_lines)]
pub fn reverse_translate_function(
    func: &Function,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    // Recognize NULLIF(json_group_array(x), '[]') and restore json_agg(x).
    // The forward translator emits this shape for both json_agg and array_agg
    // (after the R2-6 fix). The reverse cannot distinguish the two without type
    // information and consistently restores json_agg, which is the documented
    // drift. NULLIF with a json value breaks PostgreSQL because the json type
    // has no equality operator.
    if let Some(inner) = extract_json_group_array_nullif(func) {
        return build_reverse_function(
            ObjectName::from(vec![Ident::new("json_agg")]),
            inner,
            schema,
            options,
        );
    }
    // Recognize NULLIF(json_group_object(k, v), '{}') and restore
    // json_object_agg(k, v). Same NULL-over-empty semantics; same NULLIF
    // breakage on PostgreSQL.
    if let Some(inner) = extract_json_group_object_nullif(func) {
        return build_reverse_function(
            ObjectName::from(vec![Ident::new("json_object_agg")]),
            inner,
            schema,
            options,
        );
    }

    match reverse_function(&func.name, &func.args, options) {
        FunctionReversal::Rename(new_name) => {
            build_reverse_function(
                ObjectName::from(vec![Ident::new(new_name)]),
                func,
                schema,
                options,
            )
        }
        FunctionReversal::ToStringAgg => {
            let reversed = build_reverse_function(
                ObjectName::from(vec![Ident::new("string_agg")]),
                func,
                schema,
                options,
            )?;
            Ok(with_default_separator(reversed))
        }
        FunctionReversal::ToChar(template) => {
            // SQLite writes the format first, PostgreSQL writes it second.
            let exprs = extract_exactly(&func.args, 2, "strftime")?;
            let timestamp =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[1], schema, options)?;
            Ok(simple_function_expr("to_char", vec![timestamp, string_literal(&template)], None))
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
        FunctionReversal::ToAsciiNullif => {
            if let FunctionArguments::List(list) = &func.args
                && list.args.len() == 1
            {
                let argument = function_arg_expr_or_err(&list.args[0])?;
                let reversed = crate::prelude::ReverseTranslator::reverse_translate(
                    argument, schema, options,
                )?;
                return Ok(simple_function_expr(
                    "NULLIF",
                    vec![simple_function_expr("ascii", vec![reversed], None), integer_literal(0)],
                    None,
                ));
            }
            Err(Error::UnsupportedSQLiteFeature("unicode requires exactly 1 argument".to_string()))
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
            // json_set(j, '$.a', v) -> jsonb_set(j, '{a}', to_jsonb(v))
            // json_insert(j, '$.a', v) -> jsonb_insert(j, '{a}', to_jsonb(v))
            // jsonb_set and jsonb_insert require the value argument to be typed
            // as jsonb. SQLite json_set accepts any value, so the value is
            // wrapped in to_jsonb(). For non-column arguments whose type cannot
            // be resolved from the schema, to_jsonb() is the consistent fallback.
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
            let raw_value =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[2], schema, options)?;
            let value_expr = simple_function_expr("to_jsonb", vec![raw_value], None);
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
            let [value, path] = exprs.as_slice() else {
                return Err(Error::UnsupportedSQLiteFeature(
                    "json_extract requires exactly 2 arguments".to_string(),
                ));
            };
            json_path_extraction(value, path, "json_extract", schema, options)
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
        FunctionReversal::JsonTypeOf => {
            // json_type(x) -> json_typeof(x) or jsonb_typeof(x) depending on the
            // argument's declared column type. PostgreSQL's json_typeof takes
            // json, jsonb_typeof takes jsonb; the wrong variant fails at the server.
            //
            // Fallback when the argument is not a plain column reference or its
            // type is absent from the schema: json_typeof, preserving the
            // original rename behaviour as the conservative choice.
            let exprs = function_argument_exprs(&func.args);
            let arg = exprs.first().copied();
            let func_name = if arg.is_some_and(|a| {
                every_declared_type_matches(a, schema, |t| t.to_ascii_lowercase().contains("jsonb"))
            }) {
                "jsonb_typeof"
            } else {
                "json_typeof"
            };

            // json_type(x, '$.a') asks for the type at a path, and both
            // PostgreSQL spellings take one argument, so the path becomes an
            // extraction around the value. This is the shape the forward
            // direction emits for the `?`, `?|` and `?&` existence operators, so
            // without it a script this crate wrote could not be read back.
            if let [value, path] = exprs.as_slice() {
                let extracted = json_path_extraction(value, path, "json_type", schema, options)?;
                return Ok(simple_function_expr(func_name, vec![extracted], None));
            }

            build_reverse_function(
                ObjectName::from(vec![Ident::new(func_name)]),
                func,
                schema,
                options,
            )
        }
        FunctionReversal::JsonArrayLength => {
            // json_array_length(json) vs jsonb_array_length(jsonb): PostgreSQL has both
            // as distinct overloads. Use the argument's declared column type to pick.
            // When the type is unknown or not jsonb, fall back to json_array_length,
            // preserving the json behavior as the conservative default.
            let exprs = function_argument_exprs(&func.args);
            let arg = exprs.first().copied();
            let target = if arg.is_some_and(|a| {
                every_declared_type_matches(a, schema, |t| t.to_ascii_lowercase().contains("jsonb"))
            }) {
                "jsonb_array_length"
            } else {
                "json_array_length"
            };
            build_reverse_function(
                ObjectName::from(vec![Ident::new(target)]),
                func,
                schema,
                options,
            )
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
            // hex(x) -> upper(encode(x::bytea, 'hex'))
            //
            // encode() requires its first argument to be bytea. A text column
            // fails at the server with "function encode(text, unknown) does not
            // exist". Cast unconditionally: if the column is already bytea the
            // cast is a no-op, and for non-column arguments where the type cannot
            // be resolved from the schema this is the consistent fallback.
            //
            // The fold is what keeps the value: SQLite's hex answers uppercase
            // and PostgreSQL's encode answers lowercase, both measured, so the
            // bare call would quietly change the case of every digit.
            let exprs = extract_exactly(&func.args, 1, "hex")?;
            let inner =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            let bytea_cast = Expr::Cast {
                expr: Box::new(inner),
                data_type: DataType::Bytea,
                format: None,
                kind: CastKind::DoubleColon,
            };
            let encoded =
                simple_function_expr("encode", vec![bytea_cast, string_literal("hex")], None);
            Ok(simple_function_expr("upper", vec![encoded], None))
        }
        FunctionReversal::ToDecodeHex => {
            // unhex(x) -> decode(x, 'hex')
            let exprs = extract_exactly(&func.args, 1, "unhex")?;
            let inner =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            Ok(simple_function_expr("decode", vec![inner, string_literal("hex")], None))
        }
        FunctionReversal::ToExtractEpoch => {
            // unixepoch(x) and unixepoch(x, 'subsec') -> EXTRACT(EPOCH FROM x).
            // The forward direction emits the second form, since the first
            // drops the fraction.
            let exprs = function_argument_exprs(&func.args);
            let value = match exprs.as_slice() {
                // unixepoch() is the current time, and SQLite answers it as
                // whole seconds, which is why the floor and the cast are here:
                // `extract(epoch from now())` carries a fraction.
                [] => return Ok(current_epoch_seconds()),
                [value] => value,
                [
                    value,
                    Expr::Value(ValueWithSpan {
                        value: Value::SingleQuotedString(modifier), ..
                    }),
                ] if modifier == "subsec" => value,
                _ => {
                    return Err(Error::UnsupportedSQLiteFeature(
                        "unixepoch reverses as unixepoch(), unixepoch(x) or \
                         unixepoch(x, 'subsec'). Any other modifier has no PostgreSQL form."
                            .to_string(),
                    ));
                }
            };
            let inner =
                crate::prelude::ReverseTranslator::reverse_translate(*value, schema, options)?;
            Ok(Expr::Extract {
                field: DateTimeField::Epoch,
                syntax: ExtractSyntax::From,
                expr: Box::new(inner),
            })
        }
        FunctionReversal::ToSessionVariable(mapping) => {
            session_variable::reverse_expression(&mapping)
        }
        FunctionReversal::ToCast(data_type) => {
            let name = session_variable::function_name_lower(&func.name);
            let exprs = extract_exactly(&func.args, 1, &name)?;
            let inner =
                crate::prelude::ReverseTranslator::reverse_translate(exprs[0], schema, options)?;
            Ok(Expr::Cast {
                expr: Box::new(inner),
                data_type,
                format: None,
                kind: CastKind::DoubleColon,
            })
        }
        FunctionReversal::ToBareKeyword(keyword) => {
            Ok(Expr::Function(Function {
                name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(keyword))]),
                uses_odbc_syntax: false,
                parameters: FunctionArguments::None,
                args: FunctionArguments::None,
                filter: None,
                null_treatment: None,
                over: None,
                within_group: vec![],
            }))
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

    use super::{SQLITE_ONLY, reverse_translate_function};
    use crate::{
        impls::{function_helpers::function_arg_expr_or_err, timezone::is_fixed_utc_offset},
        prelude::Pg2SqliteOptions,
        traits::TranslationOptions,
    };

    /// The passthrough is decided before the reasoned refusal, so a name in
    /// both lists would slip past the reason it was written for. The two cannot
    /// overlap while every PostgreSQL-only name is absent from SQLite and every
    /// name here is a SQLite one, and this is what says so.
    #[test]
    fn no_sqlite_only_name_is_in_a_postgres_inventory() {
        for (name, _) in SQLITE_ONLY {
            assert!(
                !{
                    let class = crate::impls::sqlite_functions::classify(name);
                    class.postgres_only || class.shared_with_postgres
                },
                "{name} carries a reason it cannot cross, so it must not also pass through"
            );
        }
    }

    /// Forward rename pairs whose inverse is deliberately absent from
    /// `REVERSE_RENAMES`, each with the arm that answers for it instead.
    ///
    /// An entry here is a claim about the main match, so moving or deleting
    /// the named arm must come back to this list.
    const FORWARD_PAIRS_INVERTED_ELSEWHERE: &[(&str, &str)] = &[
        // instr reverses through the main match as POSITION(sub IN str),
        // an equivalent PostgreSQL spelling rather than strpos.
        ("strpos", "the `instr` arm emits POSITION"),
        // char(n) reverses through the main match as chr(n).
        ("chr", "the `char` arm emits chr"),
        // trim is a name both engines share, so the reverse direction passes
        // it through (two-argument form) or never sees it (one-argument TRIM
        // parses as Expr::Trim).
        ("btrim", "the shared name trim needs no reverse rename"),
        // json_array_length reverses type-sensitively: jsonb_array_length for
        // a jsonb column, passthrough otherwise.
        ("jsonb_array_length", "the `json_array_length` arm consults the schema"),
    ];

    /// Reverse rename pairs whose inverse is deliberately absent from
    /// `FORWARD_RENAMES`, each naming the forward treatment that replaces it.
    const REVERSE_PAIRS_INVERTED_ELSEWHERE: &[(&str, &str)] = &[
        // json_agg forwards as the NULLIF(json_group_array(x), '[]') idiom,
        // not a rename, so an empty set answers NULL like PostgreSQL.
        ("json_group_array", "FunctionTranslation::JsonAgg"),
        // json_object_agg forwards as the NULLIF(..., '{}') idiom.
        ("json_group_object", "FunctionTranslation::JsonObjectAgg"),
        // quote_nullable forwards through the Quote treatment, which differs
        // from a rename on NULL and on numbers.
        ("quote", "FunctionTranslation::Quote"),
        // to_jsonb forwards as a conversion, not a reinterpretation.
        ("json_quote", "FunctionTranslation::ToJson"),
        // COALESCE is a name both engines share, so the forward direction
        // passes it through.
        ("ifnull", "the shared name COALESCE needs no forward rename"),
    ];

    /// Every plain rename must invert: for a forward `(pg, sqlite)` pair the
    /// reverse table carries `(sqlite, pg)`, or the pair sits in
    /// `FORWARD_PAIRS_INVERTED_ELSEWHERE` naming what answers instead, and the
    /// mirror holds for reverse pairs. A rename deleted or added on one side
    /// alone fails here instead of silently breaking one direction, which is
    /// the drift class the `ascii`/`unicode` empty-string defect hid in.
    #[test]
    fn every_plain_rename_inverts_or_names_its_replacement() {
        let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);

        for (pg, sqlite) in crate::impls::translator_impls::function::FORWARD_RENAMES {
            let inverted = super::REVERSE_RENAMES.iter().any(|(s, p)| eq(s, sqlite) && eq(p, pg));
            let excepted = FORWARD_PAIRS_INVERTED_ELSEWHERE.iter().any(|(name, _)| eq(name, pg));
            assert!(
                inverted != excepted,
                "FORWARD_RENAMES ({pg}, {sqlite}): needs exactly one of a REVERSE_RENAMES \
                 inverse or an exception entry, found inverted={inverted} excepted={excepted}"
            );
        }

        for (sqlite, pg) in super::REVERSE_RENAMES {
            let inverted = crate::impls::translator_impls::function::FORWARD_RENAMES
                .iter()
                .any(|(p, s)| eq(p, pg) && eq(s, sqlite));
            let excepted =
                REVERSE_PAIRS_INVERTED_ELSEWHERE.iter().any(|(name, _)| eq(name, sqlite));
            assert!(
                inverted != excepted,
                "REVERSE_RENAMES ({sqlite}, {pg}): needs exactly one of a FORWARD_RENAMES \
                 inverse or an exception entry, found inverted={inverted} excepted={excepted}"
            );
        }
    }

    /// A duplicated key would make the first entry shadow the second, so the
    /// linear scans stay honest only while every key is unique.
    #[test]
    fn rename_tables_carry_no_duplicate_keys() {
        let mut forward: Vec<&str> = crate::impls::translator_impls::function::FORWARD_RENAMES
            .iter()
            .map(|(pg, _)| *pg)
            .collect();
        forward.sort_unstable();
        let before = forward.len();
        forward.dedup();
        assert_eq!(before, forward.len(), "duplicate key in FORWARD_RENAMES");

        let mut reverse: Vec<&str> = super::REVERSE_RENAMES.iter().map(|(s, _)| *s).collect();
        reverse.sort_unstable();
        let before = reverse.len();
        reverse.dedup();
        assert_eq!(before, reverse.len(), "duplicate key in REVERSE_RENAMES");
    }

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
        // The subject here is the passthrough plumbing, so the name is declared:
        // an undeclared one is refused, which `unknown_names_refuse` pins.
        let options = Pg2SqliteOptions::default().with_user_defined_functions(["custom_fn"]);
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
        let options = Pg2SqliteOptions::default().with_user_defined_functions(["custom_fn"]);
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
