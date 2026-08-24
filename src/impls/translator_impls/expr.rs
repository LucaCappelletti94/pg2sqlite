//! Implementation of the [`Translator`] trait for the
//! `Expr` type.

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

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    AccessExpr, Array, BinaryOperator, CaseWhen, CastKind, DataType, DateTimeField,
    ExactNumberInfo, Expr, Function, Ident, Interval, JsonKeyUniqueness, JsonPredicateType,
    ObjectName, ObjectNamePart, Query, SelectItem, SetExpr, Subscript, TableAlias, TableFactor,
    UnaryOperator, Value, ValueWithSpan, helpers::attached_token::AttachedToken,
};

use crate::{
    impls::{
        datetime_helpers::{DatePartKey, build_date_part_expr, datetime_field_key},
        expr_helpers::{case_when, not_predicate, null_safe_eq, null_safe_neq, rebuild},
        function_helpers::{
            integer_literal, integer_literal_value, number_literal, simple_function_expr,
            single_quoted_literal, string_literal,
        },
        idioms::wrap_with_lower,
        interval::{interval_date_modifiers, interval_date_modifiers_scaled},
        query_builder::{
            from_relation, plain_table_factor, single_expr_query, table_function_factor,
        },
        session_variable,
        shared_helpers::{
            declared_numeric_precision, every_declared_type_matches, extract_columns_from_function,
            function_argument_exprs, is_integral_expression, numeric_scale, rescale_minor_units,
            scale_decimal_literal, translate_expr_recursive,
        },
        temporal_arithmetic::{epoch_of_temporal_difference, translate_temporal_binary_op},
        timezone::{
            TimestampAwareness, flipped_shifting_offset, normalize_timezone_modifier_for_sqlite,
            timestamp_awareness,
        },
        translator_impls::{
            array::{
                Quantifier, array_concat, array_overlap, is_json_array_representation,
                json_array_call, representation_required, translate_array_literal,
                translate_array_subscript, translate_quantified_over_array,
            },
            data_type::{MAX_NUMERIC_PRECISION, exact_numeric_info, numeric_precision_and_scale},
            function::cube_root_closed_form,
            helpers::Forward,
        },
    },
    prelude::{Pg2SqliteOptions, Translator},
    traits::TranslationOptions,
};

/// Extract the search query string from a to_tsquery expression.
fn extract_query_from_tsquery(func: &Function) -> Option<String> {
    // to_tsquery can have 1 or 2 args: to_tsquery('query') or to_tsquery('config',
    // 'query'). The query is always the last expression argument.
    for expr in function_argument_exprs(&func.args).into_iter().rev() {
        if let Some(text) = single_quoted_literal(expr) {
            return Some(text.to_string());
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
fn translate_fts_expression(
    tsvector_func: &Function,
    tsquery_func: &Function,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let columns = extract_columns_from_function(tsvector_func);
    let table = schema
        .tables()
        .find(|table| {
            if columns.is_empty() {
                return false;
            }
            let Ok(table_column_iter) = table.columns(schema) else { return false };
            let table_columns: alloc::collections::BTreeSet<_> =
                table_column_iter.map(|c| c.column_name().to_lowercase()).collect();
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

    // Gate the rewrite on the column having a declared GIN / GiST
    // `to_tsvector(...)` index in the same translation unit. Without
    // this guard, querying a column with no FTS5 index emitted a
    // SELECT against a non-existent `<table>_fts` vtable that
    // runtime-errored with "no such table".
    let missing_index = !columns.iter().any(|col| options.has_fts_index(&table_name, col));
    if missing_index {
        let cols_joined = columns.join(", ");
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "FTS5 index over column(s) `{cols_joined}` in table `{table_name}` is not declared. \
             Add `CREATE INDEX <name> ON {table_name} USING GIN (to_tsvector('<lang>', \
             {first_col}))` to the schema to enable the `@@ to_tsquery(...)` rewrite.",
            first_col = columns.first().map_or("<col>", String::as_str),
        )));
    }

    let pk_columns: Vec<_> = table.primary_key_columns(schema)?.collect();
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
        subquery: Box::new(single_expr_query(
            Expr::Identifier(Ident::new("rowid")),
            from_relation(plain_table_factor(ObjectName(vec![ObjectNamePart::Identifier(
                Ident::new(fts_table_name.clone()),
            )]))),
            Some(Expr::BinaryOp {
                left: Box::new(Expr::Identifier(Ident::new(fts_table_name))),
                op: BinaryOperator::Match,
                right: Box::new(Expr::Value(ValueWithSpan {
                    value: Value::SingleQuotedString(fts5_query),
                    span: sqlparser::tokenizer::Span::empty(),
                })),
            }),
        )),
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
             YEAR, MONTH, DAY, HOUR, MINUTE, SECOND, DOW, DOY, EPOCH, WEEK, ISODOW, ISOYEAR."
        ))
    })?;
    // `extract(epoch from (a - b))` asks for the seconds in a difference
    // PostgreSQL answers as an interval, which erases the interval before it
    // needs a value of its own.
    if key == DatePartKey::Epoch
        && let Some(result) = epoch_of_temporal_difference(expr, schema, options)
    {
        return result;
    }
    Ok(build_date_part_expr(key, expr.translate(schema, options)?))
}
/// True when `expr` is an array: an `ARRAY[...]` literal, or a column declared
/// with an array type.
///
/// Under the JSON array representation an array column is TEXT holding a JSON
/// array, so nothing about the value distinguishes it from a string and the
/// declared type is the only evidence there is.
fn is_array_expression(expr: &Expr, schema: &ParserDB) -> bool {
    match expr {
        Expr::Array(_) => true,
        Expr::Nested(inner) => is_array_expression(inner, schema),
        _ => {
            every_declared_type_matches(expr, schema, |data_type| {
                let lowered = data_type.to_ascii_lowercase();
                lowered.ends_with("[]") || lowered.starts_with("array")
            })
        }
    }
}

/// False only for a literal, which is the one thing known not to be NULL
/// without running the query.
fn can_be_null(expr: &Expr) -> bool {
    !matches!(expr, Expr::Value(ValueWithSpan { value, .. }) if !matches!(value, Value::Null))
}

/// Translate a cast to boolean, which SQLite has no type for.
///
/// `CAST('true' AS INTEGER)` is 0, so mapping the target to INTEGER, right for
/// a column declaration, turns every spelling PostgreSQL accepts into false.
///
/// The accepted set is every unambiguous prefix of `true`, `false`, `yes`,
/// `no`, `on`, and `off`, plus `1` and `0`, case insensitive and trimmed. `of`
/// is in it, `o` is not, since it could be either `on` or `off`.
fn translate_boolean_cast(
    expr: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    const TRUE_SPELLINGS: [&str; 9] = ["t", "tr", "tru", "true", "y", "ye", "yes", "on", "1"];
    const FALSE_SPELLINGS: [&str; 10] =
        ["f", "fa", "fal", "fals", "false", "n", "no", "of", "off", "0"];

    if let Some(text) = single_quoted_literal(expr) {
        let spelling = text.trim().to_ascii_lowercase();
        if TRUE_SPELLINGS.contains(&spelling.as_str()) {
            return Ok(number_literal("1"));
        }
        if FALSE_SPELLINGS.contains(&spelling.as_str()) {
            return Ok(number_literal("0"));
        }
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "invalid input syntax for type boolean: \"{text}\". PostgreSQL accepts true, false, \
             yes, no, on, off, 1, 0, and any unambiguous prefix of those words."
        )));
    }

    let value = expr.translate(schema, options)?;
    let normalized = simple_function_expr(
        "lower",
        vec![simple_function_expr("trim", vec![value.clone()], None)],
        None,
    );

    Ok(Expr::Case {
        case_token: AttachedToken::empty(),
        end_token: AttachedToken::empty(),
        operand: None,
        conditions: vec![
            // Without this the NULL falls through every IN, which answers NULL
            // rather than true, and lands on the error branch.
            CaseWhen {
                condition: Expr::IsNull(Box::new(value.clone())),
                result: Expr::Value(Value::Null.with_empty_span()),
            },
            // A number is not read as text: PostgreSQL takes any nonzero
            // integer as true, where the text set would refuse 5.
            CaseWhen {
                condition: string_set_membership(
                    simple_function_expr("typeof", vec![value.clone()], None),
                    &["integer", "real"],
                ),
                result: Expr::BinaryOp {
                    left: Box::new(value.clone()),
                    op: BinaryOperator::NotEq,
                    right: Box::new(number_literal("0")),
                },
            },
            CaseWhen {
                condition: string_set_membership(normalized.clone(), &TRUE_SPELLINGS),
                result: number_literal("1"),
            },
            CaseWhen {
                condition: string_set_membership(normalized, &FALSE_SPELLINGS),
                result: number_literal("0"),
            },
        ],
        // SQLite has no way to raise from an expression, so this borrows one:
        // a JSON path that cannot parse. The message it prints carries
        // PostgreSQL's own wording and the offending value, behind a `bad JSON
        // path:` prefix. Answering NULL instead would be the silent wrongness
        // this whole rewrite exists to remove.
        else_result: Some(Box::new(simple_function_expr(
            "json_extract",
            vec![
                string_literal("{}"),
                Expr::BinaryOp {
                    left: Box::new(string_literal("invalid input syntax for type boolean: ")),
                    op: BinaryOperator::StringConcat,
                    right: Box::new(value),
                },
            ],
            None,
        ))),
    })
}

/// The word PostgreSQL renders a boolean literal as, when `expr` is one.
fn boolean_literal_word(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::Nested(inner) => boolean_literal_word(inner),
        Expr::Value(ValueWithSpan { value: Value::Boolean(value), .. }) => {
            Some(if *value { "true" } else { "false" })
        }
        _ => None,
    }
}

/// True when `expr` is boolean, either by construction or by the declared type
/// of the column it names.
///
/// SQLite has no boolean type, so a translated boolean is the integer 1 or 0
/// and nothing downstream can tell it from a count. Anything this does not
/// recognise is left alone, which keeps a value that merely happens to be 1
/// from being rendered as a word.
fn is_boolean_expression(expr: &Expr, schema: &ParserDB) -> bool {
    match expr {
        Expr::Nested(inner) => is_boolean_expression(inner, schema),
        Expr::Cast { data_type, .. } => {
            matches!(data_type, DataType::Boolean | DataType::Bool)
        }
        // Boolean by construction, whatever the operands are.
        Expr::Value(ValueWithSpan { value: Value::Boolean(_), .. })
        | Expr::UnaryOp { op: UnaryOperator::Not, .. }
        | Expr::IsNull(_)
        | Expr::IsNotNull(_)
        | Expr::IsTrue(_)
        | Expr::IsNotTrue(_)
        | Expr::IsFalse(_)
        | Expr::IsNotFalse(_)
        | Expr::IsUnknown(_)
        | Expr::IsNotUnknown(_)
        | Expr::IsDistinctFrom(_, _)
        | Expr::IsNotDistinctFrom(_, _)
        | Expr::InList { .. }
        | Expr::InSubquery { .. }
        | Expr::InUnnest { .. }
        | Expr::Between { .. }
        | Expr::Like { .. }
        | Expr::ILike { .. }
        | Expr::SimilarTo { .. }
        | Expr::RLike { .. }
        | Expr::AnyOp { .. }
        | Expr::AllOp { .. }
        | Expr::Exists { .. } => true,
        Expr::BinaryOp { op, .. } => {
            matches!(
                op,
                BinaryOperator::Eq
                    | BinaryOperator::NotEq
                    | BinaryOperator::Lt
                    | BinaryOperator::LtEq
                    | BinaryOperator::Gt
                    | BinaryOperator::GtEq
                    | BinaryOperator::And
                    | BinaryOperator::Or
                    | BinaryOperator::Xor
            )
        }
        _ => {
            every_declared_type_matches(expr, schema, |declared| {
                matches!(declared.to_ascii_lowercase().as_str(), "boolean" | "bool")
            })
        }
    }
}

/// Renders a boolean operand the way PostgreSQL renders one as text.
///
/// PostgreSQL writes the words, so `CAST(TRUE AS TEXT)` is `'true'` and
/// `'x' || TRUE` is `'xtrue'`, while the translated integer gave `'1'` and
/// `'x1'`. A literal folds straight to its word. Anything else becomes a CASE
/// with no ELSE, so a NULL boolean stays NULL rather than reading as false.
///
/// The condition is the value's own truthiness rather than a comparison
/// against 1, because a translated boolean column is a bare `INTEGER` with no
/// CHECK and can hold any integer. Measured on 3.46.0: over 1, 0, NULL and 5
/// this answers true, false, NULL and true, matching PostgreSQL's truthiness,
/// where `CASE x WHEN 1 ... WHEN 0 ...` answers NULL for the 5.
fn render_boolean_as_text(
    expr: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    if let Some(word) = boolean_literal_word(expr) {
        return Ok(string_literal(word));
    }
    let value = expr.translate(schema, options)?;
    Ok(Expr::Case {
        case_token: AttachedToken::empty(),
        end_token: AttachedToken::empty(),
        operand: None,
        conditions: vec![
            CaseWhen { condition: value.clone(), result: string_literal("true") },
            CaseWhen { condition: not_predicate(value), result: string_literal("false") },
        ],
        else_result: None,
    })
}

/// Builds `expr IN ('a', 'b', ...)`.
fn string_set_membership(expr: Expr, values: &[&str]) -> Expr {
    Expr::InList {
        expr: Box::new(expr),
        list: values.iter().map(|value| string_literal(value)).collect(),
        negated: false,
    }
}

/// Which way [`translate_integral_rounding`] moves a value that truncation
/// does not already answer correctly.
#[derive(Clone, Copy)]
enum RoundingDirection {
    /// `FLOOR`, toward negative infinity.
    Down,
    /// `CEIL`, toward positive infinity.
    Up,
}

/// Translate PostgreSQL `FLOOR(x)` or `CEIL(x)`, neither of which SQLite has.
///
/// `CAST(x AS INTEGER)` truncates toward zero, so it is already the answer on
/// one side of zero and one off on the other. Which side depends on the
/// direction, and a value that is already integral needs no adjustment either
/// way:
///
/// ```text
/// FLOOR: CASE WHEN x >= 0 OR x = CAST(x AS INTEGER) THEN CAST(x AS INTEGER) ELSE CAST(x AS INTEGER) - 1 END
/// CEIL:  CASE WHEN x <= 0 OR x = CAST(x AS INTEGER) THEN CAST(x AS INTEGER) ELSE CAST(x AS INTEGER) + 1 END
/// ```
///
/// So `FLOOR(3.7)` is 3 and `FLOOR(-3.7)` is -4, `CEIL(3.2)` is 4 and
/// `CEIL(-3.2)` is -3.
fn translate_integral_rounding(
    expr: &Expr,
    direction: RoundingDirection,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let (truncation_is_exact, adjustment) = match direction {
        RoundingDirection::Down => (BinaryOperator::GtEq, BinaryOperator::Minus),
        RoundingDirection::Up => (BinaryOperator::LtEq, BinaryOperator::Plus),
    };

    let translated_expr = expr.translate(schema, options)?;
    let cast_to_int = Expr::Cast {
        expr: Box::new(translated_expr.clone()),
        data_type: DataType::Integer(None),
        format: None,
        kind: CastKind::Cast,
    };

    let already_correct = Expr::BinaryOp {
        left: Box::new(Expr::BinaryOp {
            left: Box::new(translated_expr.clone()),
            op: truncation_is_exact,
            right: Box::new(integer_literal(0)),
        }),
        op: BinaryOperator::Or,
        right: Box::new(Expr::BinaryOp {
            left: Box::new(translated_expr),
            op: BinaryOperator::Eq,
            right: Box::new(cast_to_int.clone()),
        }),
    };

    let adjusted = Expr::BinaryOp {
        left: Box::new(cast_to_int.clone()),
        op: adjustment,
        right: Box::new(integer_literal(1)),
    };

    Ok(case_when(already_correct, cast_to_int, Some(adjusted)))
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
    Ok(simple_function_expr("INSTR", vec![translated_in, translated_substr], None))
}

/// Translate a TRIM expression to SQLite.
///
/// PostgreSQL supports `TRIM(LEADING 'x' FROM str)`, `TRIM(TRAILING 'x' FROM
/// str)`, and `TRIM(BOTH 'x' FROM str)`.  SQLite has no such syntax. The
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
            vec![translated_expr, char_expr]
        } else {
            vec![translated_expr]
        };

        return Ok(simple_function_expr(name, args, None));
    }

    // Plain TRIM(str) or TRIM(str, chars) - pass through with translated parts.
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

/// Translate `SUBSTRING(s FROM start [FOR len])` to SQLite `SUBSTR`.
///
/// PostgreSQL numbers characters from one and treats a position below one as
/// simply absent, while `FOR len` still counts from the requested start. SQLite
/// instead reads a negative start as an offset from the end of the string, so
/// the start is clamped to one and the length is reduced by however much was
/// clipped off the front. `start` is read twice, which is only observable for a
/// volatile expression.
fn translate_substring(
    expr: &Expr,
    substring_from: Option<&Expr>,
    substring_for: Option<&Expr>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated = expr.translate(schema, options)?;
    let start = substring_from
        .map(|e| e.translate(schema, options))
        .transpose()?
        .map(|s| simple_function_expr("max", vec![s.clone(), integer_literal(1)], None));
    let length = match (substring_from, substring_for) {
        (Some(from), Some(for_len)) => {
            let from = from.translate(schema, options)?;
            let for_len = for_len.translate(schema, options)?;
            // Characters before position one do not exist, so drop as many of
            // them from the requested length as the start was short by.
            let clipped = Expr::BinaryOp {
                left: Box::new(Expr::BinaryOp {
                    left: Box::new(for_len),
                    op: BinaryOperator::Plus,
                    right: Box::new(simple_function_expr(
                        "min",
                        vec![from, integer_literal(1)],
                        None,
                    )),
                }),
                op: BinaryOperator::Minus,
                right: Box::new(integer_literal(1)),
            };
            Some(simple_function_expr("max", vec![clipped, integer_literal(0)], None))
        }
        (None, Some(for_len)) => Some(for_len.translate(schema, options)?),
        (_, None) => None,
    };

    Ok(Expr::Substring {
        expr: Box::new(translated),
        substring_from: start.map(Box::new),
        substring_for: length.map(Box::new),
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

/// Convert a PostgreSQL text-array path literal `{a,b}` to a SQLite JSON path
/// `$.a.b`.
///
/// Unquoted keys are used as-is. Double-quoted keys (e.g. `"a b"`) have their
/// outer quotes stripped. Returns `None` when the outer braces are missing.
pub(crate) fn pg_text_path_to_sqlite_json_path(s: &str) -> Option<String> {
    let inner = s.strip_prefix('{')?.strip_suffix('}')?;
    if inner.is_empty() {
        return Some("$".to_string());
    }
    let mut path = String::from("$");
    for key in inner.split(',') {
        let key = key.trim();
        let key = if key.starts_with('"') && key.ends_with('"') && key.len() >= 2 {
            &key[1..key.len() - 1]
        } else {
            key
        };
        path.push('.');
        path.push_str(key);
    }
    Some(path)
}

/// Convert a SQLite JSON path `$.a.b` to a PostgreSQL text-array path `{a,b}`.
///
/// Only handles simple dotted-key paths produced by the forward translator.
/// Returns `None` for paths that contain array-index steps (`$[0]`), bare `$`
/// with no key, or any other shape that cannot round-trip through text-array
/// notation.
pub(crate) fn sqlite_json_path_to_pg_text_path(s: &str) -> Option<String> {
    let rest = s.strip_prefix("$.")?;
    let keys: Vec<&str> = rest.split('.').collect();
    if keys.is_empty() || keys.iter().any(|k| k.is_empty() || k.contains('[')) {
        return None;
    }
    Some(format!("{{{}}}", keys.join(",")))
}

/// Extract a slice of string key literals from an `ARRAY[...]` expression.
///
/// Returns `None` when the expression is not an array literal or when any
/// element is not a single-quoted string literal. The keys are extracted
/// before array translation so the original string values are available.
fn extract_string_array_keys(expr: &Expr) -> Option<Vec<&str>> {
    let Expr::Array(Array { elem, .. }) = expr else {
        return None;
    };
    elem.iter().map(single_quoted_literal).collect()
}

/// The distinct comparisons, handed to SQLite unchanged.
///
/// SQLite has taken both spellings since 3.39, under the 3.46 floor. They used
/// to be lowered onto its bare `IS`, which `sqlparser` cannot read back.
fn translate_distinct_comparison(
    left: &Expr,
    right: &Expr,
    is_not_distinct: bool,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let l = left.translate(schema, options)?;
    let r = right.translate(schema, options)?;
    Ok(if is_not_distinct { null_safe_eq(l, r) } else { null_safe_neq(l, r) })
}

/// One element of a PostgreSQL `text[]` path literal, after quoting is gone.
enum JsonPathElement {
    /// Integer-shaped, so PostgreSQL reads it by the runtime shape of the
    /// value at that depth: an index when it is an array, a key otherwise.
    /// `index` is the converted number and `verbatim` the element text, which
    /// differ for a spelling like `01`.
    Numeric { index: i64, verbatim: String },
    /// A key and nothing else.
    Key(String),
    /// An unquoted `NULL`, which is a NULL element rather than a key.
    Null,
}

/// Reads a PostgreSQL `text[]` literal like `{a, "b,c", 0}` into its elements.
///
/// The rules, measured against 18.4 rather than recalled: elements split on
/// top-level commas, whitespace around an unquoted element is trimmed, a
/// double-quoted element is taken verbatim with `\x` unescaping to `x`, and an
/// unquoted `NULL` in any letter case is a NULL element. `None` for anything
/// malformed, which PostgreSQL refuses as a literal too.
fn parse_text_array_path(literal: &str) -> Option<Vec<JsonPathElement>> {
    let inner = literal.trim().strip_prefix('{')?.strip_suffix('}')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }

    let mut elements = Vec::new();
    let mut chars = inner.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let element = if chars.peek() == Some(&'"') {
            chars.next();
            let mut text = String::new();
            loop {
                match chars.next()? {
                    '"' => break,
                    '\\' => text.push(chars.next()?),
                    other => text.push(other),
                }
            }
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            JsonPathElement::Key(text)
        } else {
            let mut text = String::new();
            while chars.peek().is_some_and(|c| *c != ',') {
                let c = chars.next()?;
                // A quote or brace inside an unquoted element is not a text[]
                // literal PostgreSQL would take either.
                if matches!(c, '"' | '{' | '}' | '\\') {
                    return None;
                }
                text.push(c);
            }
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            if text.eq_ignore_ascii_case("null") {
                JsonPathElement::Null
            } else {
                classify_path_text(text)
            }
        };
        elements.push(element);

        match chars.next() {
            Some(',') => {}
            None => return Some(elements),
            Some(_) => return None,
        }
    }
}

/// Whether an element is integer-shaped, which decides the emitted form. A
/// quoted `"0"` classifies the same way, because the quoting is literal syntax
/// and PostgreSQL's path logic sees only the value.
fn classify_path_text(text: &str) -> JsonPathElement {
    match text.parse::<i64>() {
        Ok(index) => JsonPathElement::Numeric { index, verbatim: text.to_string() },
        Err(_) => JsonPathElement::Key(text.to_string()),
    }
}

/// PostgreSQL's `#>` and `#>>`, read into SQLite's `->`/`->>` chains.
///
/// `x #> '{a,b}'` becomes `x -> 'a' -> 'b'`, and the text form takes `->>` on
/// the last hop only. Both arrows are native SQLite since 3.38, under the
/// floor, and this crate already passes chains of them through unchanged.
///
/// A numeric element is the one place a single arrow cannot be faithful.
/// PostgreSQL decides by the runtime value: `'{"0":"x"}'::jsonb #> '{0}'`
/// answers the key and `'[1,2]'::jsonb #> '{0}'` the index, both measured.
/// SQLite's integer arrow indexes arrays only and its string arrow reads keys
/// only, each answering NULL on the other shape, so
/// `COALESCE(x -> 0, x -> '0')` reproduces the decision: whichever arm matches
/// the shape answers, and the other is NULL. The arms cannot disagree, because
/// an arrow on the wrong shape never answers at all.
///
/// The empty path is the document itself: the operand for `#>`, and its text
/// for `#>>`, which `->> '$'` answers. A composite document's text differs in
/// whitespace between the engines, the same divergence the plain `->>`
/// passthrough already carries; a scalar's does not, measured.
fn translate_json_path_operator(
    left: &Expr,
    right: &Expr,
    text_form: bool,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let operator = if text_form { "#>>" } else { "#>" };
    let Some(literal) = single_quoted_literal(right) else {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "`{operator}` needs its path as a literal. PostgreSQL accepts a computed one, and \
             the path decides which keys are read, so it cannot be rewritten without its value."
        )));
    };
    let Some(elements) = parse_text_array_path(literal) else {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "`{literal}` is not a path literal `{operator}` can read: PostgreSQL takes a text \
             array like '{{a,b}}', with a double-quoted element for a key containing a comma, \
             a brace or a quote."
        )));
    };

    let mut value = left.translate(schema, options)?;
    if elements.is_empty() {
        // The document itself, and its text for the text form.
        if text_form {
            value = Expr::BinaryOp {
                left: Box::new(value),
                op: BinaryOperator::LongArrow,
                right: Box::new(string_literal("$")),
            };
        }
        return Ok(value);
    }

    let last = elements.len() - 1;
    for (position, element) in elements.into_iter().enumerate() {
        let op = if text_form && position == last {
            BinaryOperator::LongArrow
        } else {
            BinaryOperator::Arrow
        };
        let hop = |value: Expr, key: Expr| {
            Expr::BinaryOp { left: Box::new(value), op: op.clone(), right: Box::new(key) }
        };
        value = match element {
            JsonPathElement::Key(key) => hop(value, string_literal(&key)),
            JsonPathElement::Numeric { index, verbatim } => {
                simple_function_expr(
                    "COALESCE",
                    vec![
                        hop(value.clone(), integer_literal(index)),
                        hop(value, string_literal(&verbatim)),
                    ],
                    None,
                )
            }
            JsonPathElement::Null => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "the path `{literal}` contains a NULL element, which PostgreSQL answers \
                     NULL for; write the path without NULL."
                )));
            }
        };
    }
    Ok(value)
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
        simple_function_expr("length", vec![translated_overlay_what.clone()], None)
    };

    let prefix = simple_function_expr(
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
        None,
    );

    let suffix = simple_function_expr(
        "substr",
        vec![
            translated_expr,
            Expr::BinaryOp {
                left: Box::new(translated_overlay_from),
                op: BinaryOperator::Plus,
                right: Box::new(replacement_len),
            },
        ],
        None,
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
#[cfg(all(test, feature = "std"))]
fn is_sqlite_fixed_offset(value: &str) -> bool {
    crate::impls::timezone::is_fixed_utc_offset(value)
}

/// Normalize PostgreSQL AT TIME ZONE literal names to SQLite datetime
/// modifiers.
fn normalize_at_time_zone_modifier(value: &str) -> Option<String> {
    normalize_timezone_modifier_for_sqlite(value)
}

/// `CAST(x AS NUMERIC(p,s))`, which moves `x` onto the scaled-integer
/// representation rather than truncating it into a bare INTEGER.
fn translate_numeric_cast(
    expr: &Expr,
    info: &ExactNumberInfo,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let (_, target_scale) = numeric_precision_and_scale(info)?;
    let translated = expr.translate(schema, options)?;

    if let Some(source_scale) = numeric_scale(expr, schema) {
        return Ok(rescale_minor_units(translated, source_scale, target_scale));
    }

    // An operand that is not already minor units, an integer column or a
    // literal, is at scale 0 by definition, so it only needs multiplying up.
    // Anything whose scale cannot be resolved would be shifted by a guess.
    if is_integral_expression(expr, schema) {
        return Ok(rescale_minor_units(translated, 0, target_scale));
    }
    Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
        "CAST({expr} AS NUMERIC) needs the operand's scale, since a NUMERIC column is emitted as \
         an INTEGER of minor units and the cast has to know how far to move the point. The type \
         of `{expr}` cannot be resolved here. Cast it to a declared NUMERIC first."
    )))
}

/// Translate PostgreSQL `expr AT TIME ZONE '...'` to a SQLite `datetime` call.
///
/// The two PostgreSQL operations share this syntax and shift opposite ways: a
/// naive timestamp moves toward UTC, an aware one away from it. Over
/// `2023-01-15 12:00:00` with `'+05:30'` PostgreSQL answers 17:30 and 06:30.
///
/// The naive side ADDS because PostgreSQL reads a bare `'+05:30'` string as a
/// POSIX zone, whose sign is the opposite of the ISO one. `AT TIME ZONE
/// INTERVAL '05:30'` goes the other way and is not accepted here.
fn translate_at_time_zone(
    timestamp: &Expr,
    time_zone: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_timestamp = timestamp.translate(schema, options)?;

    let modifier = single_quoted_literal(time_zone)
        .and_then(normalize_at_time_zone_modifier)
        .ok_or_else(|| {
        crate::errors::Error::UnsupportedSQLiteFeature(
            "AT TIME ZONE supports only literal UTC/local or fixed offsets (+HH:MM/-HH:MM) in SQLite translation."
                .to_string(),
        )
    })?;

    // UTC shifts nothing in PostgreSQL whichever side the operand is on, so its
    // type need not be resolved. SQLite's own `utc` modifier is NOT that: it
    // reads the value as local time, so emitting it made the answer depend on
    // the offset of whatever machine ran the query.
    if modifier == "utc" || modifier == "+00:00" || modifier == "-00:00" {
        return Ok(simple_function_expr("datetime", vec![translated_timestamp], None));
    }

    let Some(negated) = flipped_shifting_offset(&modifier) else {
        // `localtime`, where both databases mean the machine's own zone and
        // neither agrees on which machine.
        return Ok(simple_function_expr(
            "datetime",
            vec![translated_timestamp, string_literal(&modifier)],
            None,
        ));
    };

    let awareness = timestamp_awareness(timestamp, schema).ok_or_else(|| {
        crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "AT TIME ZONE shifts a bare timestamp and a timestamptz in opposite directions, and \
             `{timestamp}` is not known to be either, so either choice would be wrong half the \
             time. Cast the operand, as `{timestamp}::timestamptz`, to say which it is."
        ))
    })?;

    let applied = match awareness {
        TimestampAwareness::Naive => modifier,
        TimestampAwareness::Aware => negated,
    };
    Ok(simple_function_expr("datetime", vec![translated_timestamp, string_literal(&applied)], None))
}

/// Translate a vector type cast to the appropriate sqlite-vec function.
///
/// - `'[1,2,3]'::vector` → `vec_f32('[1,2,3]')` (32-bit float)
/// - `'[1,2,3]'::halfvec` → `vec_f16('[1,2,3]')` (16-bit float)
///
/// The predicates come from `vector.rs`, which reads the LAST path segment of
/// the type name, so `public.vector` counts. A local copy here read the FIRST
/// segment, and the qualified spelling fell through to `CAST(... AS BLOB)`,
/// which stores the text bytes as the vector.
fn translate_vector_cast(
    expr: &Expr,
    data_type: &DataType,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_expr = expr.translate(schema, options)?;
    let func_name = if crate::impls::translator_impls::vector::is_halfvec_data_type(data_type) {
        "vec_f16"
    } else {
        "vec_f32"
    };

    Ok(simple_function_expr(func_name, vec![translated_expr], None))
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

    Ok(simple_function_expr(function_name, vec![translated_left, translated_right], None))
}

/// The elements of a quantifier operand that compares element-wise: an
/// `ARRAY[...]` literal or a parenthesized expression list. Both keep
/// PostgreSQL's three-valued logic exactly when folded, so they never need the
/// `json_each` path.
fn quantifier_elements(right: &Expr) -> Option<&[Expr]> {
    match right {
        Expr::Array(Array { elem, .. }) => Some(elem),
        Expr::Tuple(exprs) => Some(exprs),
        _ => None,
    }
}

/// Translate the equality forms `x = ANY(...)` and `x <> ALL(...)` into
/// `IN` / `NOT IN`.
///
/// Supported operands:
/// - subquery: `x = ANY(SELECT ...)` -> `x IN (SELECT ...)`
/// - array literal: `x = ANY(ARRAY[...])` -> `x IN (...)`
/// - tuple: `x = ANY((...))` -> `x IN (...)`
/// - array value: `x = ANY(tags)` -> `EXISTS (SELECT 1 FROM json_each(tags)
///   ...)`
fn translate_any_all_to_in(
    left: &Expr,
    right: &Expr,
    negated: bool,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_left = left.translate(schema, options)?;

    if let Expr::Subquery(q) = right {
        return Ok(Expr::InSubquery {
            expr: Box::new(translated_left),
            subquery: Box::new(q.translate(schema, options)?),
            negated,
        });
    }
    if let Some(elements) = quantifier_elements(right) {
        return Ok(Expr::InList {
            expr: Box::new(translated_left),
            list: elements
                .iter()
                .map(|e| e.translate(schema, options))
                .collect::<Result<Vec<_>, _>>()?,
            negated,
        });
    }

    let (compare_op, quantifier) = if negated {
        (BinaryOperator::NotEq, Quantifier::All)
    } else {
        (BinaryOperator::Eq, Quantifier::Any)
    };
    translate_quantified_over_array(
        &translated_left,
        &compare_op,
        right.translate(schema, options)?,
        quantifier,
        options,
    )
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

/// The names the quantifier lowering emits: the derived table wrapping the
/// subquery, and the projection alias the comparison references.
const QUANTIFIER_ALIAS: &str = "__pg2sqlite_quantifier";
const QUANTIFIER_ITEM_ALIAS: &str = "__pg2sqlite_item";

/// Names the quantifier subquery's output column `__pg2sqlite_item` inside
/// its projection.
///
/// This is the R105 shape: SQLite has no grammar for a column alias list on
/// a table alias, so `(SELECT id FROM t) q (item)` cannot be emitted, and
/// the alias has to ride the projection instead. A set operation's column
/// names come from its first operand, so the leftmost SELECT is the one to
/// rename. A subquery without a single expression projection (a wildcard, a
/// VALUES body) is refused naming the fix, where the old shape emitted SQL
/// SQLite cannot parse.
fn alias_quantifier_projection(query: &mut Query) -> Result<(), crate::errors::Error> {
    fn leftmost_select(set_expr: &mut SetExpr) -> Option<&mut sqlparser::ast::Select> {
        match set_expr {
            SetExpr::Select(select) => Some(select),
            SetExpr::SetOperation { left, .. } => leftmost_select(left),
            SetExpr::Query(inner) => leftmost_select(&mut inner.body),
            _ => None,
        }
    }

    let refusal = || {
        crate::errors::Error::UnsupportedSQLiteFeature(
            "ANY/ALL over this subquery cannot be translated: the rewrite names the compared \
             column inside the projection, so the subquery must project exactly one named \
             expression. Project the column explicitly, `SELECT col FROM ...`."
                .to_string(),
        )
    };

    let select = leftmost_select(&mut query.body).ok_or_else(refusal)?;
    let [item] = select.projection.as_mut_slice() else {
        // PostgreSQL itself refuses a quantifier subquery with more than one
        // column, so only the single-item shape reaches this rewrite.
        return Err(refusal());
    };
    match item {
        SelectItem::UnnamedExpr(expr) => {
            *item = SelectItem::ExprWithAlias {
                expr: expr.clone(),
                alias: Ident::new(QUANTIFIER_ITEM_ALIAS),
            };
        }
        SelectItem::ExprWithAlias { alias, .. } => *alias = Ident::new(QUANTIFIER_ITEM_ALIAS),
        _ => return Err(refusal()),
    }
    Ok(())
}

/// `EXISTS`/`NOT EXISTS` over the quantifier subquery, aliased so the compared
/// item has a name the predicate can reference.
fn build_exists_over_subquery(
    translated_left: &Expr,
    compare_op: &BinaryOperator,
    mut translated_subquery: Query,
    negate_comparison: bool,
    negate_exists: bool,
) -> Result<Expr, crate::errors::Error> {
    alias_quantifier_projection(&mut translated_subquery)?;

    let derived_alias = Ident::new(QUANTIFIER_ALIAS);
    let item_ref =
        Expr::CompoundIdentifier(vec![derived_alias.clone(), Ident::new(QUANTIFIER_ITEM_ALIAS)]);
    let mut comparison = Expr::BinaryOp {
        left: Box::new(translated_left.clone()),
        op: compare_op.clone(),
        right: Box::new(item_ref),
    };
    if negate_comparison {
        comparison = not_predicate(comparison);
    }

    Ok(Expr::Exists {
        subquery: Box::new(single_expr_query(
            integer_literal(1),
            from_relation(TableFactor::Derived {
                lateral: false,
                subquery: Box::new(translated_subquery),
                alias: Some(TableAlias {
                    explicit: false,
                    name: derived_alias,
                    columns: Vec::new(),
                    at: None,
                }),
                sample: None,
            }),
            Some(comparison),
        )),
        negated: negate_exists,
    })
}

/// Translate a non-equality quantified comparison, `x <op> ANY(...)` or
/// `x <op> ALL(...)`.
///
/// An element-wise operand folds into an `OR` chain for `ANY` and an `AND`
/// chain for `ALL`, with the empty operand collapsing to the quantifier's
/// identity. A subquery becomes an `EXISTS`. Anything else is an array value
/// and goes through `json_each`.
fn translate_quantified_operation(
    left: &Expr,
    compare_op: &BinaryOperator,
    right: &Expr,
    quantifier: Quantifier,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    let translated_left = left.translate(schema, options)?;
    let (fold_op, empty_value, negate) = match quantifier {
        Quantifier::Any => (BinaryOperator::Or, false, false),
        Quantifier::All => (BinaryOperator::And, true, true),
    };

    if let Expr::Subquery(q) = right {
        return build_exists_over_subquery(
            &translated_left,
            compare_op,
            q.translate(schema, options)?,
            negate,
            negate,
        );
    }
    if let Some(elements) = quantifier_elements(right) {
        let predicates = elements
            .iter()
            .map(|expr| {
                Ok(Expr::BinaryOp {
                    left: Box::new(translated_left.clone()),
                    op: compare_op.clone(),
                    right: Box::new(expr.translate(schema, options)?),
                })
            })
            .collect::<Result<Vec<_>, crate::errors::Error>>()?;
        return Ok(fold_predicates(predicates, &fold_op, empty_value));
    }

    translate_quantified_over_array(
        &translated_left,
        compare_op,
        right.translate(schema, options)?,
        quantifier,
        options,
    )
}

/// Apply D1's arithmetic rules to an operation whose operands are held as
/// minor units.
///
/// Addition and subtraction need one scale, so the lesser side is multiplied
/// up. Multiplication needs none: the integer product is already at the sum of
/// the scales, matching `1.50 * 2.2500 = 3.375000`. It can still overflow, so
/// the result precision is checked where both operand types are known.
///
/// Division is refused: PostgreSQL picks a result scale from both operand
/// precisions, answering `NUMERIC(10,2) / integer` at scale 20, so any scale
/// chosen here would disagree by a different amount for every operand pair.
fn translate_numeric_arithmetic(
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    (left_scale, right_scale): (u32, u32),
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Expr>, crate::errors::Error> {
    let translated = |side: &Expr| side.translate(schema, options);
    let combined = match op {
        BinaryOperator::Plus | BinaryOperator::Minus => {
            let common = left_scale.max(right_scale);
            Expr::BinaryOp {
                left: Box::new(rescale_minor_units(translated(left)?, left_scale, common)),
                op: op.clone(),
                right: Box::new(rescale_minor_units(translated(right)?, right_scale, common)),
            }
        }
        BinaryOperator::Multiply => {
            let (left_precision, right_precision) =
                (numeric_precision(left, schema), numeric_precision(right, schema));
            if let (Some(left_precision), Some(right_precision)) = (left_precision, right_precision)
                && left_precision + right_precision > MAX_NUMERIC_PRECISION
            {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "NUMERIC({left_precision},{left_scale}) * \
                     NUMERIC({right_precision},{right_scale}) needs \
                     {} digits, and a SQLite INTEGER holds at most {MAX_NUMERIC_PRECISION}. \
                     PostgreSQL gives a product the sum of the operand precisions, so the \
                     result would silently become a float. Narrow one of the operands.",
                    left_precision + right_precision
                )));
            }
            Expr::BinaryOp {
                left: Box::new(translated(left)?),
                op: op.clone(),
                right: Box::new(translated(right)?),
            }
        }
        BinaryOperator::Divide => {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "dividing NUMERIC values has no faithful SQLite form: PostgreSQL chooses the \
                 result scale from both operand precisions, answering `{left} / {right}` at a \
                 scale neither operand has, and SQLite's integer division truncates toward zero \
                 on top of that. Write the scale you want, as CAST({left} AS NUMERIC(18,6)) / \
                 ..., or do the division in the application."
            )));
        }
        _ => return Ok(None),
    };
    Ok(Some(combined))
}

/// The declared precision of a NUMERIC expression, when it has one.
fn numeric_precision(expr: &Expr, schema: &ParserDB) -> Option<u64> {
    match expr {
        Expr::Nested(inner) => numeric_precision(inner, schema),
        Expr::Cast { data_type, .. } => {
            match exact_numeric_info(data_type) {
                Some(info) => {
                    numeric_precision_and_scale(info).ok().map(|(precision, _)| precision)
                }
                None => declared_numeric_precision(expr, schema),
            }
        }
        _ => declared_numeric_precision(expr, schema),
    }
}

/// Whether the NUMERIC scale rules could bear on this operation at all.
///
/// The rules only reach arithmetic and comparison, and only when a side could
/// name a column or be a decimal literal. Checking that first keeps a schema
/// lookup out of every `AND` in every predicate, which is most of the binary
/// operators in a real query.
fn numeric_rules_may_apply(op: &BinaryOperator, left: &Expr, right: &Expr) -> bool {
    if !matches!(
        op,
        BinaryOperator::Plus
            | BinaryOperator::Minus
            | BinaryOperator::Multiply
            | BinaryOperator::Divide
            | BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq
    ) {
        return false;
    }
    let addressable = |expr: &Expr| {
        matches!(
            expr,
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Nested(_) | Expr::Cast { .. }
        )
    };
    addressable(left) || addressable(right)
}

/// `a @> b` (a contains b): every element of b must be in a.
///
/// Implemented as `NOT EXISTS (SELECT 1 FROM json_each(b) WHERE value NOT IN
/// (SELECT value FROM json_each(a)))`. An empty needle produces no rows so
/// NOT EXISTS is true, matching PostgreSQL's `{1,2} @> {}` = true.
/// Duplicates are ignored because IN membership is set-based.
fn array_containment(haystack: Expr, needle: Expr) -> Expr {
    let haystack_values = Box::new(single_expr_query(
        Expr::Identifier(Ident::new("value")),
        from_relation(table_function_factor("json_each", vec![haystack], None, false)),
        None,
    ));
    let not_in_haystack = Expr::InSubquery {
        expr: Box::new(Expr::Identifier(Ident::new("value"))),
        subquery: haystack_values,
        negated: true,
    };
    let inner = single_expr_query(
        integer_literal(1),
        from_relation(table_function_factor("json_each", vec![needle], None, false)),
        Some(not_in_haystack),
    );
    Expr::Exists { subquery: Box::new(inner), negated: true }
}

/// Extract the `Interval` node from a bare or parenthesised interval
/// expression.
fn extract_interval_expr(expr: &Expr) -> Option<&Interval> {
    match expr {
        Expr::Interval(i) => Some(i),
        Expr::Nested(inner) => {
            match inner.as_ref() {
                Expr::Interval(i) => Some(i),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Detect `scalar * INTERVAL` or `INTERVAL * scalar`. Returns (scalar_expr,
/// interval) when the pattern matches; the caller decides whether the scalar is
/// a constant.
fn scalar_times_interval(expr: &Expr) -> Option<(&Expr, &Interval)> {
    if let Expr::BinaryOp { left, op: BinaryOperator::Multiply, right } = expr {
        if let Some(i) = extract_interval_expr(right) {
            return Some((left, i));
        }
        if let Some(i) = extract_interval_expr(left) {
            return Some((right, i));
        }
    }
    None
}

/// Translate a binary operation expression.
#[allow(clippy::too_many_lines)]
fn translate_binary_op(
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    if numeric_rules_may_apply(op, left, right) {
        let scales = (numeric_scale(left, schema), numeric_scale(right, schema));
        // A NUMERIC column holds minor units, so a decimal literal beside one
        // has to be moved onto the same scale. Missing this is silent:
        // `price = 19.99` compares 1999 against 19.99 and returns nothing.
        if let Some(scale) = scales.0.filter(|scale| *scale > 0)
            && let Some(scaled) = scale_decimal_literal(right, scale)?
        {
            return Ok(Expr::BinaryOp {
                left: Box::new(left.translate(schema, options)?),
                op: op.clone(),
                right: Box::new(scaled),
            });
        }
        if let Some(scale) = scales.1.filter(|scale| *scale > 0)
            && let Some(scaled) = scale_decimal_literal(left, scale)?
        {
            return Ok(Expr::BinaryOp {
                left: Box::new(scaled),
                op: op.clone(),
                right: Box::new(right.translate(schema, options)?),
            });
        }
        if let (Some(left_scale), Some(right_scale)) = scales
            && let Some(combined) = translate_numeric_arithmetic(
                left,
                op,
                right,
                (left_scale, right_scale),
                schema,
                options,
            )?
        {
            return Ok(combined);
        }
    }

    // Check for full-text search: to_tsvector(...) @@ to_tsquery(...)
    if *op == BinaryOperator::AtAt {
        if let (Expr::Function(tsvector_func), Expr::Function(tsquery_func)) = (left, right)
            && is_to_tsvector(tsvector_func)
            && is_to_tsquery(tsquery_func)
        {
            return translate_fts_expression(tsvector_func, tsquery_func, schema, options);
        }
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
            "The @@ operator is only supported for to_tsvector(...) @@ to_tsquery(...) \
             full-text search expressions."
                .to_string(),
        ));
    }

    // PostgreSQL's #> and #>> read a value at a path. SQLite has no path
    // operator, but it has the arrows, so a literal path becomes a chain.
    if matches!(op, BinaryOperator::HashArrow | BinaryOperator::HashLongArrow) {
        return translate_json_path_operator(
            left,
            right,
            *op == BinaryOperator::HashLongArrow,
            schema,
            options,
        );
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
            other => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "The {other} operator has no SQLite translation. Emitting it would fail at \
                     prepare time, since SQLite has no such operator."
                )));
            }
        }
    }

    // ^ is exponentiation in PostgreSQL but bitwise XOR in SQLite, so passthrough
    // is wrong.
    if *op == BinaryOperator::PGExp {
        if !options.is_math_functions_available() {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "^ (PostgreSQL exponentiation) requires SQLITE_ENABLE_MATH_FUNCTIONS. \
                 Enable with .with_math_functions_available() or use pow() explicitly."
                    .to_string(),
            ));
        }
        let l = left.translate(schema, options)?;
        let r = right.translate(schema, options)?;
        return Ok(simple_function_expr("pow", vec![l, r], None));
    }

    // # is PostgreSQL bitwise XOR; SQLite has no # token.
    // (a | b) - (a & b) equals a XOR b exactly for all integers.
    if *op == BinaryOperator::PGBitwiseXor {
        let l = left.translate(schema, options)?;
        let r = right.translate(schema, options)?;
        let or_expr = Expr::Nested(Box::new(Expr::BinaryOp {
            left: Box::new(l.clone()),
            op: BinaryOperator::BitwiseOr,
            right: Box::new(r.clone()),
        }));
        let and_expr = Expr::Nested(Box::new(Expr::BinaryOp {
            left: Box::new(l),
            op: BinaryOperator::BitwiseAnd,
            right: Box::new(r),
        }));
        return Ok(Expr::BinaryOp {
            left: Box::new(or_expr),
            op: BinaryOperator::Minus,
            right: Box::new(and_expr),
        });
    }

    // `||` is overloaded. On arrays PostgreSQL concatenates elements, on text
    // it concatenates characters, and under the JSON array representation an
    // array is TEXT, so passing it through turned `{1,2} || {3,4}` into the
    // string `[1,2][3,4]`. The operands decide, not the operator.
    if *op == BinaryOperator::StringConcat
        && (is_array_expression(left, schema) || is_array_expression(right, schema))
    {
        if !is_json_array_representation(options) {
            return Err(representation_required("The || (array concatenation) operator"));
        }
        // PostgreSQL appends or prepends a lone element, and a one element
        // array expands to exactly that element, so both spellings reuse the
        // array shape rather than needing their own.
        let as_array = |side: &Expr| -> Result<Expr, crate::errors::Error> {
            let translated = side.translate(schema, options)?;
            Ok(if is_array_expression(side, schema) {
                translated
            } else {
                json_array_call(vec![translated])
            })
        };
        let concatenated = array_concat(as_array(left)?, as_array(right)?);

        // Two NULL arrays concatenate to NULL in PostgreSQL, where the rewrite
        // would answer an empty array. One NULL needs no guard: PostgreSQL
        // reads it as empty, which is what expanding it to no rows already
        // does.
        if can_be_null(left) && can_be_null(right) {
            return Ok(case_when(
                Expr::BinaryOp {
                    left: Box::new(Expr::IsNull(Box::new(left.translate(schema, options)?))),
                    op: BinaryOperator::And,
                    right: Box::new(Expr::IsNull(Box::new(right.translate(schema, options)?))),
                },
                Expr::Value(Value::Null.with_empty_span()),
                Some(concatenated),
            ));
        }
        return Ok(concatenated);
    }

    // `||` over a boolean renders it, since PostgreSQL writes the word there
    // too: `'x' || TRUE` is `xtrue`, where the translated integer gave `x1`.
    // Only the boolean side is wrapped, so a text or numeric operand keeps
    // concatenating exactly as it did.
    if *op == BinaryOperator::StringConcat
        && (is_boolean_expression(left, schema) || is_boolean_expression(right, schema))
    {
        let rendered = |side: &Expr| -> Result<Expr, crate::errors::Error> {
            if is_boolean_expression(side, schema) {
                render_boolean_as_text(side, schema, options)
            } else {
                side.translate(schema, options)
            }
        };
        return Ok(Expr::BinaryOp {
            left: Box::new(rendered(left)?),
            op: BinaryOperator::StringConcat,
            right: Box::new(rendered(right)?),
        });
    }

    // Array overlap. Rewritten over `json_each` rather than passed through,
    // which emitted `json_array(1, 2) && json_array(2, 3)` and failed at the
    // `&`. Gated on the array representation like every other array operation.
    if *op == BinaryOperator::PGOverlap {
        if !is_json_array_representation(options) {
            return Err(representation_required("The && (array overlap) operator"));
        }
        return Ok(array_overlap(
            left.translate(schema, options)?,
            right.translate(schema, options)?,
        ));
    }

    // POSIX regex operators: SQLite's REGEXP needs an application-supplied function
    // and cannot honor POSIX semantics. Match the wording used by
    // regexp_match/regexp_matches.
    match op {
        BinaryOperator::PGRegexMatch | BinaryOperator::PGRegexNotMatch => {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "~ / !~ (PostgreSQL POSIX regex) are not supported in SQLite without a REGEXP \
                 extension. For basic pattern matching use LIKE or GLOB."
                    .to_string(),
            ));
        }
        BinaryOperator::PGRegexIMatch | BinaryOperator::PGRegexNotIMatch => {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "~* / !~* (case-insensitive POSIX regex) are not supported in SQLite without a \
                 REGEXP extension, which cannot honor case-insensitive matching even when registered."
                    .to_string(),
            ));
        }
        // Array containment (@> / <@). When both operands resolve to arrays under
        // the JSON representation, rewrite via NOT EXISTS / json_each anti-join.
        // For jsonb operands the refusal names jsonb so the message does not
        // mislead array callers who forgot to set ArrayRepresentation::Json.
        BinaryOperator::AtArrow => {
            if is_json_array_representation(options)
                && (is_array_expression(left, schema) || is_array_expression(right, schema))
            {
                // a @> b: every element of b must be in a.
                return Ok(array_containment(
                    left.translate(schema, options)?,
                    right.translate(schema, options)?,
                ));
            }
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "@> (jsonb containment) is not supported in SQLite. PostgreSQL jsonb \
                 containment is recursive partial-match; json_each cannot express it without \
                 a recursive CTE per nesting level. For array columns use \
                 ArrayRepresentation::Json."
                    .to_string(),
            ));
        }
        BinaryOperator::ArrowAt => {
            if is_json_array_representation(options)
                && (is_array_expression(left, schema) || is_array_expression(right, schema))
            {
                // a <@ b: every element of a is in b (same as b @> a).
                return Ok(array_containment(
                    right.translate(schema, options)?,
                    left.translate(schema, options)?,
                ));
            }
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "<@ (jsonb contained-by) is not supported in SQLite. PostgreSQL jsonb \
                 containment is recursive partial-match; json_each cannot express it without \
                 a recursive CTE per nesting level. For array columns use \
                 ArrayRepresentation::Json."
                    .to_string(),
            ));
        }
        // doc ? 'k' -> json_type(doc, '$."k"') IS NOT NULL
        BinaryOperator::Question => {
            return rebuild(|| {
                let translated_doc = left.translate(schema, options)?;
                let key = match right {
                    Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(s), .. }) => {
                        s.clone()
                    }
                    _ => {
                        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                            "? (key-exists) requires a single-quoted string literal key on the \
                             right-hand side."
                                .to_string(),
                        ));
                    }
                };
                let path = format!("$.\"{}\"", key);
                let call = simple_function_expr(
                    "json_type",
                    vec![translated_doc, string_literal(&path)],
                    None,
                );
                Ok(Expr::IsNotNull(Box::new(call)))
            });
        }
        // doc ?| ARRAY[...] -> OR chain of json_type IS NOT NULL
        // doc ?& ARRAY[...] -> AND chain of json_type IS NOT NULL
        BinaryOperator::QuestionPipe | BinaryOperator::QuestionAnd => {
            return rebuild(|| {
                let translated_doc = left.translate(schema, options)?;
                let keys = extract_string_array_keys(right).ok_or_else(|| {
                    crate::errors::Error::UnsupportedSQLiteFeature(
                        "?| / ?& require an ARRAY literal of string literals on the right-hand side; \
                         the key list must be known at translation time to build json_type() paths."
                            .to_string(),
                    )
                })?;
                let is_all = matches!(op, BinaryOperator::QuestionAnd);
                let predicates: Vec<Expr> = keys
                    .iter()
                    .map(|k| {
                        let path = format!("$.\"{}\"", k);
                        let call = simple_function_expr(
                            "json_type",
                            vec![translated_doc.clone(), string_literal(&path)],
                            None,
                        );
                        Expr::IsNotNull(Box::new(call))
                    })
                    .collect();
                let fold_op = if is_all { BinaryOperator::And } else { BinaryOperator::Or };
                Ok(fold_predicates(predicates, &fold_op, is_all))
            });
        }
        // doc #- '{a,b}' -> json_remove(doc, '$.a.b')
        // Reuses the same {a,b} -> $.a.b path conversion used by #> / #>>.
        BinaryOperator::HashMinus => {
            return rebuild(|| {
                let translated_doc = left.translate(schema, options)?;
                let path = match right {
                    Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(s), .. }) => {
                        pg_text_path_to_sqlite_json_path(s).ok_or_else(|| {
                            crate::errors::Error::UnsupportedSQLiteFeature(
                                "#- path must be a '{key1,key2}' PostgreSQL text-array literal."
                                    .to_string(),
                            )
                        })?
                    }
                    _ => {
                        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                            "#- path must be a '{key1,key2}' PostgreSQL text-array literal."
                                .to_string(),
                        ));
                    }
                };
                Ok(simple_function_expr(
                    "json_remove",
                    vec![translated_doc, string_literal(&path)],
                    None,
                ))
            });
        }
        // s ^@ p is exact prefix comparison with no pattern semantics, so a
        // LIKE translation would misread % and _ in the prefix.
        BinaryOperator::PGStartsWith => {
            return rebuild(|| {
                let translated_left = left.translate(schema, options)?;
                let translated_right = right.translate(schema, options)?;
                let prefix_len =
                    simple_function_expr("length", vec![translated_right.clone()], None);
                let head = simple_function_expr(
                    "substr",
                    vec![translated_left, integer_literal(1), prefix_len],
                    None,
                );
                Ok(Expr::BinaryOp {
                    left: Box::new(head),
                    op: BinaryOperator::Eq,
                    right: Box::new(translated_right),
                })
            });
        }
        // jsonpath has no json1 counterpart, so refuse rather than emit the
        // operator SQLite cannot tokenise.
        BinaryOperator::AtQuestion => {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "@? (jsonpath exists) is not supported: SQLite's json1 has no jsonpath engine. \
                 Rewrite the check as json_type(doc, '$.path') IS NOT NULL with a SQLite JSON \
                 path."
                    .to_string(),
            ));
        }
        // OPERATOR(pg_catalog.<op>) is PostgreSQL's schema-qualified operator
        // spelling. A known operator unwraps and re-dispatches through this
        // function, so it keeps its specific handling (pow for ^, the POSIX
        // regex refusal for ~). An operator this crate cannot name is refused
        // rather than emitted, since SQLite has no OPERATOR() grammar.
        BinaryOperator::PGCustomBinaryOperator(parts) => {
            let Some(plain) = unwrap_pg_catalog_operator(parts) else {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "OPERATOR({}) has no SQLite translation: only pg_catalog operators with a \
                     plain spelling can be unwrapped.",
                    parts.join(".")
                )));
            };
            return translate_binary_op(left, &plain, right, schema, options);
        }
        _ => {}
    }

    // PG INTERVAL arithmetic: `target + INTERVAL 'N unit'` becomes
    // `datetime(target, '+M months', 'floor', '+D days', '+S seconds')`, and
    // `-` negates every count.
    //
    // Three shapes beyond the basic right-hand-interval case:
    // 1. INTERVAL + target: commute to target + INTERVAL.
    // 2. n * INTERVAL or INTERVAL * n: fold the literal integer scalar.
    // 3. Unrecognised interval notation (HH:MM:SS, ISO P-forms): emit a message
    //    naming the verbose form instead of blaming INTERVAL wholesale.
    if matches!(op, BinaryOperator::Plus | BinaryOperator::Minus) {
        let negating = matches!(op, BinaryOperator::Minus);

        // Case 1: INTERVAL on the left commutes (addition only).
        if *op == BinaryOperator::Plus && extract_interval_expr(left).is_some() {
            return translate_binary_op(right, op, left, schema, options);
        }

        // Case 2: n * INTERVAL or INTERVAL * n on the right.
        if let Some((scalar, interval)) = scalar_times_interval(right) {
            match integer_literal_value(scalar) {
                Some(n) => {
                    match interval_date_modifiers_scaled(interval, negating, n)? {
                        Some(modifiers) => {
                            let target = left.translate(schema, options)?;
                            let mut args = vec![target];
                            for modifier in modifiers {
                                args.push(string_literal(&modifier));
                            }
                            return Ok(simple_function_expr("datetime", args, None));
                        }
                        None => {
                            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                                "INTERVAL '{}' notation is not supported. Use the verbose form                                  with explicit count and unit pairs, for example                                  INTERVAL '1 hour 30 minutes'.",
                                single_quoted_literal(interval.value.as_ref()).unwrap_or("...")
                            )));
                        }
                    }
                }
                None => {
                    // Runtime scalar: cannot be constant-folded into a modifier.
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                        "INTERVAL scaled by a runtime expression cannot be folded at translation                          time. Compute the multiplier in application code and use a plain                          INTERVAL literal, for example INTERVAL '3 days'."
                            .to_string(),
                    ));
                }
            }
        }

        // Case 3: bare INTERVAL on the right.
        if let Some(interval) = extract_interval_expr(right) {
            match interval_date_modifiers(interval, negating)? {
                Some(modifiers) => {
                    let target = left.translate(schema, options)?;
                    let mut args = vec![target];
                    for modifier in modifiers {
                        args.push(string_literal(&modifier));
                    }
                    return Ok(simple_function_expr("datetime", args, None));
                }
                None => {
                    // The notation was not decoded (HH:MM:SS, ISO P-form, 'ago').
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                        "INTERVAL '{}' notation is not supported. Use the verbose form with                          explicit count and unit pairs, for example INTERVAL '1 hour 30 minutes'.",
                        single_quoted_literal(interval.value.as_ref()).unwrap_or("...")
                    )));
                }
            }
        }
    }

    // Every other `+`/`-` over a date, a timestamp or a time. Left alone it
    // reaches SQLite as arithmetic over the text those values are held in.
    if let Some(result) = translate_temporal_binary_op(left, op, right, schema, options) {
        return result;
    }

    Ok(Expr::BinaryOp {
        left: Box::new(left.translate(schema, options)?),
        op: op.clone(),
        right: Box::new(right.translate(schema, options)?),
    })
}

/// Maps the operator inside `OPERATOR(pg_catalog.<op>)` to its plain spelling.
///
/// A single-segment path is accepted too, since `OPERATOR(+)` means the same
/// search-path lookup landing on the built-in operator.
fn unwrap_pg_catalog_operator(parts: &[String]) -> Option<BinaryOperator> {
    let name = match parts {
        [op] => op.as_str(),
        [schema, op] if schema.eq_ignore_ascii_case("pg_catalog") => op.as_str(),
        _ => return None,
    };
    Some(match name {
        "+" => BinaryOperator::Plus,
        "-" => BinaryOperator::Minus,
        "*" => BinaryOperator::Multiply,
        "/" => BinaryOperator::Divide,
        "%" => BinaryOperator::Modulo,
        "=" => BinaryOperator::Eq,
        "<>" | "!=" => BinaryOperator::NotEq,
        "<" => BinaryOperator::Lt,
        "<=" => BinaryOperator::LtEq,
        ">" => BinaryOperator::Gt,
        ">=" => BinaryOperator::GtEq,
        "||" => BinaryOperator::StringConcat,
        "&" => BinaryOperator::BitwiseAnd,
        "|" => BinaryOperator::BitwiseOr,
        "#" => BinaryOperator::PGBitwiseXor,
        "^" => BinaryOperator::PGExp,
        "<<" => BinaryOperator::PGBitwiseShiftLeft,
        ">>" => BinaryOperator::PGBitwiseShiftRight,
        "~" => BinaryOperator::PGRegexMatch,
        "~*" => BinaryOperator::PGRegexIMatch,
        "!~" => BinaryOperator::PGRegexNotMatch,
        "!~*" => BinaryOperator::PGRegexNotIMatch,
        _ => return None,
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
            Expr::BinaryOp { left, op, right } => {
                translate_binary_op(left, op, right, schema, options)?
            }
            Expr::Cast { expr, data_type, format, .. } => {
                rebuild(|| {
                    // A cast over the caller's identity, which the replica's
                    // function answers without one. Checked before the vector
                    // and numeric paths, since the pattern decides the shape
                    // rather than the type written over it.
                    if let Some(paired) =
                        session_variable::translate_cast(expr, data_type, options)?
                    {
                        return Ok(paired);
                    }
                    if crate::impls::translator_impls::vector::is_vector_data_type(data_type) {
                        return translate_vector_cast(expr, data_type, schema, options);
                    }
                    // PG `'...'::uuid` under Blob representation: lower to the
                    // same text-to-blob expression used for INSERT/UPDATE wraps.
                    // Without this branch the cast would emit invalid
                    // `'...'::BLOB` SQLite syntax via the generic Cast path.
                    if matches!(data_type, sqlparser::ast::DataType::Uuid)
                        && crate::impls::translator_impls::uuid::is_blob_uuid_representation(
                            options,
                        )
                    {
                        let translated = expr.translate(schema, options)?;
                        // The literal wrapper validates and canonicalises, and
                        // passes anything else through, so the cast and the
                        // INSERT path accept and refuse exactly the same set.
                        let wrapped =
                            crate::impls::translator_impls::uuid::maybe_wrap_text_uuid_literal(
                                translated.clone(),
                                options,
                            )?;
                        return Ok(if wrapped == translated {
                            crate::impls::translator_impls::uuid::make_uuid_conversion_call(
                                translated, options,
                            )
                        } else {
                            wrapped
                        });
                    }
                    // SQLite has no cast format, so a `FORMAT` clause cannot be
                    // honored and cloning it through produced SQL SQLite rejects at
                    // parse time.
                    if let Some(format) = format {
                        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                            "CAST(... AS {data_type} FORMAT {format}) is not supported in SQLite, \
                         which has no cast format clause. Format the value with strftime() or \
                         printf() instead."
                        )));
                    }
                    if matches!(
                        data_type,
                        sqlparser::ast::DataType::Boolean | sqlparser::ast::DataType::Bool
                    ) {
                        return translate_boolean_cast(expr, schema, options);
                    }
                    if let Some(info) = exact_numeric_info(data_type) {
                        return translate_numeric_cast(expr, info, schema, options);
                    }
                    let translated_type = data_type.translate(schema, options)?;
                    // A boolean rendered as text reads `true` or `false` in
                    // PostgreSQL, and the translated integer would give `1`.
                    if matches!(translated_type, sqlparser::ast::DataType::Text)
                        && is_boolean_expression(expr, schema)
                    {
                        return render_boolean_as_text(expr, schema, options);
                    }
                    // SQLite only accepts the `CAST(x AS type)` spelling, not
                    // PostgreSQL's `x::type` operator nor TRY_CAST / SAFE_CAST, so
                    // force `CastKind::Cast` regardless of how the source wrote it.
                    Ok(Expr::Cast {
                        expr: Box::new(expr.translate(schema, options)?),
                        data_type: translated_type,
                        format: None,
                        kind: CastKind::Cast,
                    })
                })?
            }
            Expr::AtTimeZone { timestamp, time_zone } => {
                translate_at_time_zone(timestamp, time_zone, schema, options)?
            }
            // UNKNOWN is rewritten as a NULL check on the boolean expression result.
            Expr::IsUnknown(inner) => Expr::IsNull(Box::new(inner.translate(schema, options)?)),
            Expr::IsNotUnknown(inner) => {
                Expr::IsNotNull(Box::new(inner.translate(schema, options)?))
            }
            Expr::IsDistinctFrom(left, right) => {
                translate_distinct_comparison(left, right, false, schema, options)?
            }
            Expr::IsNotDistinctFrom(left, right) => {
                translate_distinct_comparison(left, right, true, schema, options)?
            }
            Expr::Like { negated, any, expr, pattern, escape_char } => {
                rebuild(|| -> Result<Expr, crate::errors::Error> {
                    Ok(Expr::Like {
                        negated: *negated,
                        any: *any,
                        expr: Box::new(expr.translate(schema, options)?),
                        pattern: Box::new(pattern.translate(schema, options)?),
                        escape_char: sqlite_like_escape(escape_char.clone()),
                    })
                })?
            }
            Expr::ILike { negated, any, expr, pattern, escape_char } => {
                rebuild(|| -> Result<Expr, crate::errors::Error> {
                    let translated_expr = expr.translate(schema, options)?;
                    let translated_pattern = pattern.translate(schema, options)?;
                    let escape = sqlite_like_escape(lowered_ilike_escape(escape_char.as_ref())?);
                    if let Some(fold_fn) = options.get_ilike_fold_function() {
                        // Use the caller-provided fold function instead of lower().
                        let fold = |e| simple_function_expr(fold_fn, vec![e], None);
                        return Ok(Expr::Like {
                            negated: *negated,
                            any: *any,
                            expr: Box::new(fold(translated_expr)),
                            pattern: Box::new(fold(translated_pattern)),
                            escape_char: escape,
                        });
                    }
                    // No fold function. Refuse a literal pattern with non-ASCII
                    // alphabetic characters: SQLite lower() is ASCII-only and
                    // would produce silent wrong results.
                    if has_non_ascii_alpha_literal(pattern) {
                        return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                            "ILIKE with a pattern containing non-ASCII alphabetic characters \
                             cannot translate faithfully because SQLite lower() folds ASCII \
                             only. Declare a Unicode-capable fold function via \
                             .with_ilike_fold_function() to enable non-ASCII ILIKE."
                                .to_string(),
                        ));
                    }
                    Ok(Expr::Like {
                        negated: *negated,
                        any: *any,
                        expr: Box::new(wrap_with_lower(translated_expr)),
                        pattern: Box::new(wrap_with_lower(translated_pattern)),
                        escape_char: escape,
                    })
                })?
            }
            Expr::Extract { field, expr, .. } => translate_extract(field, expr, schema, options)?,
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
            Expr::Ceil { expr, .. } => {
                translate_integral_rounding(expr, RoundingDirection::Up, schema, options)?
            }
            Expr::Floor { expr, .. } => {
                translate_integral_rounding(expr, RoundingDirection::Down, schema, options)?
            }
            Expr::Position { expr, r#in } => translate_position(expr, r#in, schema, options)?,
            Expr::Substring { expr, substring_from, substring_for, .. } => {
                translate_substring(
                    expr,
                    substring_from.as_deref(),
                    substring_for.as_deref(),
                    schema,
                    options,
                )?
            }
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
            Expr::TypedString(typed_string) => {
                rebuild(|| -> Result<Expr, crate::errors::Error> {
                    Ok(Expr::Cast {
                        expr: Box::new(Expr::Value(typed_string.value.clone())),
                        data_type: typed_string.data_type.translate(schema, options)?,
                        format: None,
                        kind: sqlparser::ast::CastKind::Cast,
                    })
                })?
            }
            Expr::Prefixed { value, .. } => value.translate(schema, options)?,
            Expr::Collate { expr, collation } => {
                rebuild(|| -> Result<Expr, crate::errors::Error> {
                    Ok(Expr::Collate {
                        expr: Box::new(expr.translate(schema, options)?),
                        collation: sqlite_collation(collation)?,
                    })
                })?
            }
            Expr::Interval(interval) => {
                let field_str =
                    interval.leading_field.as_ref().map_or(String::new(), |f| format!(" {f}"));
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "INTERVAL{field_str} expressions are not supported in SQLite. \
                     Use SQLite date modifiers instead: \
                     date('now', '-7 days'), datetime('now', '+1 hour'), etc."
                )));
            }
            Expr::IsNormalized { .. } => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "IS NORMALIZED (Unicode normalization check) is not supported in SQLite. \
                     Consider using application-level normalization with ICU or a similar library."
                        .to_string(),
                ));
            }
            Expr::AnyOp { left, compare_op, right, .. } => {
                if matches!(compare_op, BinaryOperator::Eq) {
                    return translate_any_all_to_in(left, right, false, schema, options);
                }
                translate_quantified_operation(
                    left,
                    compare_op,
                    right,
                    Quantifier::Any,
                    schema,
                    options,
                )?
            }
            Expr::AllOp { left, compare_op, right } => {
                if matches!(compare_op, BinaryOperator::NotEq) {
                    return translate_any_all_to_in(left, right, true, schema, options);
                }
                translate_quantified_operation(
                    left,
                    compare_op,
                    right,
                    Quantifier::All,
                    schema,
                    options,
                )?
            }
            Expr::IsJson { expr, kind, unique_keys, negated } => {
                translate_is_json(expr, *kind, *unique_keys, *negated, schema, options)?
            }
            Expr::Array(Array { elem, .. }) => translate_array_literal(elem, schema, options)?,
            Expr::CompoundFieldAccess { root, access_chain } => {
                translate_compound_field_access(root, access_chain, schema, options)?
            }
            Expr::SimilarTo { .. } => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "SIMILAR TO is not supported in SQLite. \
                     Consider using LIKE for simple patterns or application-level regex matching."
                        .to_string(),
                ));
            }
            Expr::Rollup(_) | Expr::Cube(_) | Expr::GroupingSets(_) => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "ROLLUP / CUBE / GROUPING SETS are not supported in SQLite. \
                     Restructure as separate GROUP BY queries and UNION ALL the results."
                        .to_string(),
                ));
            }
            Expr::InUnnest { .. } => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "IN UNNEST (array unnesting) is not supported in SQLite. \
                     Use a subquery with a VALUES clause or a temporary table instead."
                        .to_string(),
                ));
            }
            Expr::Lambda(_) => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "Lambda expressions are not supported in SQLite.".to_string(),
                ));
            }
            Expr::MatchAgainst { .. } => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "MATCH...AGAINST is MySQL-specific syntax and is not supported in SQLite. \
                     Use SQLite FTS5: table MATCH 'query'."
                        .to_string(),
                ));
            }
            Expr::OuterJoin(_) => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "Oracle outer join syntax (+) is not supported in SQLite. \
                     Use standard SQL LEFT JOIN / RIGHT JOIN syntax."
                        .to_string(),
                ));
            }
            Expr::Prior(_) => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "PRIOR (Oracle hierarchical query syntax) is not supported in SQLite. \
                     Use recursive CTEs (WITH RECURSIVE) for hierarchical queries."
                        .to_string(),
                ));
            }
            // PG-specific prefix operators that SQLite lacks.
            // Remaining UnaryOp variants (Not, Minus, Plus, BitwiseNot) fall through
            // to translate_expr_recursive, which keeps them and recurses into the operand.
            Expr::UnaryOp { op, expr } => {
                rebuild(|| {
                    Ok(match op {
                        UnaryOperator::PGSquareRoot => {
                            if !options.is_math_functions_available() {
                                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                                    "|/ (square root) requires SQLITE_ENABLE_MATH_FUNCTIONS. \
                             Enable with .with_math_functions_available()."
                                        .to_string(),
                                ));
                            }
                            simple_function_expr(
                                "sqrt",
                                vec![expr.translate(schema, options)?],
                                None,
                            )
                        }
                        UnaryOperator::PGCubeRoot => {
                            if !options.is_math_functions_available() {
                                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                                    "||/ (cube root) requires SQLITE_ENABLE_MATH_FUNCTIONS. \
                             Enable with .with_math_functions_available()."
                                        .to_string(),
                                ));
                            }
                            let x = expr.translate(schema, options)?;
                            cube_root_closed_form(x)
                        }
                        UnaryOperator::PGAbs => {
                            simple_function_expr(
                                "abs",
                                vec![expr.translate(schema, options)?],
                                None,
                            )
                        }
                        _ => translate_expr_recursive::<Forward>(self, schema, options)?,
                    })
                })?
            }
            // All remaining variants: delegate structural recursion to shared helper.
            // This covers: Identifier, CompoundIdentifier, Value, Nested,
            // IsNull, IsNotNull, IsTrue/IsFalse/IsNotTrue/IsNotFalse, Exists,
            // InList, InSubquery, Between, Case, Subquery, Tuple,
            // RLike, JsonAccess, QualifiedWildcard, Struct,
            // Named, Dictionary, Map, MemberOf, etc.
            _ => translate_expr_recursive::<Forward>(self, schema, options)?,
        })
    }
}

/// Lowers an `ILIKE` escape character with the operands.
///
/// The `ILIKE` rewrite folds both operands through `lower()`, so a letter
/// escape left unfolded no longer occurs in the lowered pattern and stops
/// escaping: an escaped literal becomes a live wildcard and the reverse, with
/// no error anywhere, measured on both databases under `ESCAPE 'X'`. A
/// character whose lowering is not exactly one character would shift the
/// pattern instead of escaping in it, so it is refused. Anything that is not
/// a one-character single-quoted string is left verbatim: PostgreSQL rejects
/// it at run time and SQLite rejects the emission the same way, unchanged by
/// this fold.
fn lowered_ilike_escape(
    escape_char: Option<&ValueWithSpan>,
) -> Result<Option<ValueWithSpan>, crate::errors::Error> {
    let Some(escape) = escape_char else { return Ok(None) };
    let Value::SingleQuotedString(original) = &escape.value else {
        return Ok(Some(escape.clone()));
    };
    let mut characters = original.chars();
    let (Some(_), None) = (characters.next(), characters.next()) else {
        return Ok(Some(escape.clone()));
    };

    let lowered = original.to_lowercase();
    if lowered.chars().count() != 1 {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "ILIKE ... ESCAPE '{original}' cannot be translated: ILIKE becomes LIKE over \
             lower()ed operands, and lowercasing this escape character changes its length, \
             which would shift the pattern instead of escaping in it. Use a caseless escape \
             character such as a backslash."
        )));
    }

    Ok(Some(ValueWithSpan { value: Value::SingleQuotedString(lowered), span: escape.span }))
}

/// True when `expr` is a string literal containing at least one character that
/// is alphabetic but not ASCII. Used to gate the ILIKE refusal: SQLite
/// `lower()` only folds ASCII, so a non-ASCII alphabetic pattern produces
/// silent wrong answers.
fn has_non_ascii_alpha_literal(expr: &Expr) -> bool {
    let mut e = expr;
    while let Expr::Nested(inner) = e {
        e = inner.as_ref();
    }
    let Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(s), .. }) = e else {
        return false;
    };
    s.chars().any(|c| c.is_alphabetic() && !c.is_ascii())
}

/// The escape character a translated `LIKE` carries.
///
/// PostgreSQL's `LIKE` escapes with a backslash unless the statement names
/// another character, while SQLite's has no escape character at all until one
/// is named, so `'100%' LIKE '100\%'` is true on one engine and false on the
/// other. Naming the backslash restores the PostgreSQL reading and changes
/// nothing for a pattern that holds no backslash, and it does not cost the
/// index scan that `LIKE 'abc%'` gets, measured on 3.46.0 and 3.51.1.
///
/// PostgreSQL spells "no escape character" as `ESCAPE ''`, which is what
/// SQLite's bare `LIKE` already means, so that clause is dropped rather than
/// forwarded: SQLite refuses the empty spelling with `ESCAPE expression must
/// be a single character`.
fn sqlite_like_escape(escape_char: Option<ValueWithSpan>) -> Option<ValueWithSpan> {
    match &escape_char {
        None => {
            Some(ValueWithSpan {
                value: Value::SingleQuotedString("\\".to_string()),
                span: sqlparser::tokenizer::Span::empty(),
            })
        }
        Some(escape) if escape.value == Value::SingleQuotedString(String::new()) => None,
        Some(_) => escape_char,
    }
}

/// Maps a PostgreSQL collation name onto the SQLite collation that orders the
/// same way, and reports the names that have none.
///
/// `C` and `POSIX` are byte order collations with no locale behind them.
/// Measured on PostgreSQL 16, both sort `A,B,Zz,_z,a,b`, which is what SQLite
/// `BINARY` gives, and `pg_collation` reports both as deterministic. Every
/// other PostgreSQL name is locale dependent, so it has no SQLite counterpart
/// and no ordering this can promise.
pub(crate) fn sqlite_collation(collation: &ObjectName) -> Result<ObjectName, crate::errors::Error> {
    let name = collation
        .0
        .last()
        .and_then(ObjectNamePart::as_ident)
        .map(|ident| ident.value.to_ascii_uppercase())
        .unwrap_or_default();

    match name.as_str() {
        "BINARY" | "NOCASE" | "RTRIM" => Ok(collation.clone()),
        "C" | "POSIX" => Ok(ObjectName(vec![ObjectNamePart::Identifier(Ident::new("BINARY"))])),
        _ => {
            Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "COLLATE {name} is not a valid SQLite collation. SQLite only supports BINARY \
                 (default), NOCASE, and RTRIM, and PostgreSQL C and POSIX map onto BINARY. \
                 Any other PostgreSQL collation name must be dropped or replaced before \
                 translation."
            )))
        }
    }
}

/// Translate `<expr> IS [NOT] JSON [VALUE|SCALAR|ARRAY|OBJECT]` onto json1.
///
/// `json_valid` answers the unqualified and `VALUE` forms directly. The shape
/// predicates need `json_type`, which raises on malformed input, so the
/// `json_valid` guard has to be a `CASE` rather than an `AND`: SQLite does not
/// promise to short-circuit `AND`.
///
/// `WITH`/`WITHOUT UNIQUE KEYS` has no json1 equivalent. json1 keeps the last
/// of a set of duplicate keys with no way to observe that it did, so the
/// predicate is rejected rather than answered wrongly.
fn translate_is_json(
    expr: &Expr,
    kind: Option<JsonPredicateType>,
    unique_keys: Option<JsonKeyUniqueness>,
    negated: bool,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    if let Some(unique_keys) = unique_keys {
        return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
            "IS JSON ... {unique_keys} is not supported in SQLite. The json1 extension keeps the \
             last of a set of duplicate object keys and offers no way to detect that it did, so \
             the uniqueness constraint cannot be checked."
        )));
    }

    let translated = expr.translate(schema, options)?;
    let is_valid = simple_function_expr("json_valid", vec![translated.clone()], None);

    let predicate = match kind {
        // `IS JSON` and `IS JSON VALUE` both ask whether the text parses.
        None | Some(JsonPredicateType::Value) => is_valid,
        Some(shape) => {
            let type_of = simple_function_expr("json_type", vec![translated], None);
            let shape_test = match shape {
                JsonPredicateType::Array | JsonPredicateType::Object => {
                    let name = if shape == JsonPredicateType::Array { "array" } else { "object" };
                    Expr::BinaryOp {
                        left: Box::new(type_of),
                        op: BinaryOperator::Eq,
                        right: Box::new(string_literal(name)),
                    }
                }
                // Anything that is neither a container nor absent is a scalar.
                JsonPredicateType::Scalar => {
                    Expr::InList {
                        expr: Box::new(type_of),
                        list: vec![string_literal("array"), string_literal("object")],
                        negated: true,
                    }
                }
                JsonPredicateType::Value => unreachable!("handled above"),
            };
            case_when(is_valid, shape_test, Some(boolean_literal(false)))
        }
    };

    Ok(if negated { not_predicate(predicate) } else { predicate })
}

/// Translate a compound field access chain.
///
/// A single one-based array subscript maps onto a JSON path extraction. Slices
/// and multi-step chains do not: a slice would have to rebuild an array, and a
/// nested subscript would have to re-parse the JSON text `json_extract` returns
/// for a container element.
fn translate_compound_field_access(
    root: &Expr,
    access_chain: &[AccessExpr],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, crate::errors::Error> {
    match access_chain {
        [AccessExpr::Subscript(Subscript::Index { index })] => {
            let translated_root = root.translate(schema, options)?;
            translate_array_subscript(translated_root, index, schema, options)
        }
        [AccessExpr::Subscript(Subscript::Slice { .. })] => {
            Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "Array slice subscripting (arr[lo:hi]) is not supported in SQLite. Rebuild the \
                 slice with json_each() and json_group_array(), or slice in the application."
                    .to_string(),
            ))
        }
        // Plain `a.b.c` field paths carry no subscript and need no rewriting.
        _ if !access_chain.iter().any(|a| matches!(a, AccessExpr::Subscript(_))) => {
            translate_expr_recursive::<Forward>(
                &Expr::CompoundFieldAccess {
                    root: Box::new(root.clone()),
                    access_chain: access_chain.to_vec(),
                },
                schema,
                options,
            )
        }
        _ => {
            Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "Chained subscript access is not supported in SQLite. json_extract() returns a \
                 nested element as JSON text, so a second subscript would index the text rather \
                 than the element. Extract a single level at a time."
                    .to_string(),
            ))
        }
    }
}

#[cfg(all(test, feature = "std"))]
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
        extract_query_from_tsquery, is_sqlite_fixed_offset, normalize_at_time_zone_modifier,
        translate_any_all_to_in, translate_extract, translate_trim,
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

        // EXTRACT(WEEK) is the ISO week, which SQLite spells %V. %W is the
        // Sunday based one and disagrees at every year boundary.
        let week = translate_extract(
            &DateTimeField::Week(None),
            &parse_expr("created_at"),
            &schema,
            &options,
        )
        .expect("week extract should translate");
        assert!(week.to_string().contains("'%V'"), "week must use the ISO format: {week}");

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

        // TRIM(LEADING FROM str) - no char → LTRIM(str)
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
        .expect_err("ANY over an array value should need an array representation");
        assert!(err.to_string().contains("with_array_representation"), "got: {err}");

        let access = Expr::CompoundFieldAccess {
            root: Box::new(parse_expr("payload")),
            access_chain: vec![
                AccessExpr::Subscript(Subscript::Index { index: parse_expr("1") }),
                AccessExpr::Dot(parse_expr("value")),
            ],
        };
        let chained = access
            .translate(&schema, &options)
            .expect_err("chained subscript access should be rejected");
        assert!(chained.to_string().contains("Chained subscript access"), "got: {chained}");

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
