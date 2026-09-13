//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `Column` type.

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
    CheckConstraint, ColumnDef, ColumnOption, ColumnOptionDef, DataType, Expr, Ident, ObjectName,
    TimezoneInfo, UnaryOperator, Value, ValueWithSpan,
};

use crate::{
    errors::Error,
    impls::{
        object_name::last_ident_value_or_display,
        shared_helpers::{minor_unit_scale, scale_decimal_literal},
        translator_impls::{
            data_type::{
                bit_exact_length_check_expr, bit_length, bit_max_length_check_expr,
                character_length, character_length_bound_expr, exact_numeric_info, is_serial_type,
                numeric_precision_and_scale, numeric_precision_bound_expr,
                regular_int_range_bound_expr, small_int_range_bound_expr,
            },
            uuid::{
                is_blob_uuid_representation, is_uuid_data_type, uuid_blob_length_check_expr,
                wrap_uuid_column_default,
            },
        },
    },
    prelude::Pg2SqliteOptions,
    traits::translator::TranslatorWithContext,
};

/// Rewrites a scaled `NUMERIC` column's declared `DEFAULT` as minor units, or
/// answers `None` for a default that cannot land as one number at the
/// column's scale.
///
/// PostgreSQL coerces the default to the column's type when the table is
/// created, so a bare number, a quoted number, and a parenthesised number are
/// all the same literal, measured on PostgreSQL 16 where `DEFAULT '1.50'`
/// reads back 1.50. `NULL` survives untouched, since an absent value has no
/// scale. Anything else, arithmetic or a function call, would need evaluating
/// at translate time, and PostgreSQL itself rejects a malformed string here
/// with `invalid input syntax for type numeric`.
fn scaled_numeric_default(expr: &Expr, scale: u32) -> Result<Option<Expr>, Error> {
    let mut peeled = expr;
    while let Expr::Nested(inner) = peeled {
        peeled = inner;
    }

    if let Some(scaled) = scale_decimal_literal(peeled, scale)? {
        return Ok(Some(scaled));
    }

    match peeled {
        Expr::Value(ValueWithSpan { value: Value::Null, .. }) => Ok(Some(peeled.clone())),
        Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(text), .. }) => {
            match quoted_decimal_as_number(text) {
                Some(number) => scale_decimal_literal(&number, scale),
                None => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

/// Reads a quoted decimal, `'1.50'` or `' -2.5 '`, back as the number literal
/// it coerces to, or `None` when the text is not one number.
fn quoted_decimal_as_number(text: &str) -> Option<Expr> {
    let trimmed = text.trim();
    let unsigned = trimmed.strip_prefix(['-', '+']).unwrap_or(trimmed);
    let shape_holds = !unsigned.is_empty()
        && unsigned.chars().all(|c| c.is_ascii_digit() || c == '.')
        && unsigned.chars().filter(|c| *c == '.').count() <= 1
        && unsigned.chars().any(|c| c.is_ascii_digit());
    if !shape_holds {
        return None;
    }

    let digits =
        if trimmed.starts_with('-') { format!("-{unsigned}") } else { unsigned.to_string() };
    Some(Expr::Value(ValueWithSpan {
        value: Value::Number(digits, false),
        span: sqlparser::tokenizer::Span::empty(),
    }))
}

/// The `DEFAULT` a column declares, in the units the translated column stores.
///
/// One accessor because two emitters need the same answer: the column
/// definition, which carries it into the table, and the RLS INSERT trigger,
/// which has to reproduce it because a SQLite view holds no defaults. A
/// `NUMERIC(10,2) DEFAULT 1.5` column stores minor units, so both have to read
/// 150. A UUID BLOB column's text-literal default must become the same
/// binary-conversion expression an explicit insert of that literal would use,
/// or the trigger and the table definition diverge.
///
/// # Errors
///
/// Returns [`Error::TranslationRefusal`] when a scaled `NUMERIC` column's
/// default is not one number at the column's scale, or when a UUID BLOB
/// column's default is a text literal that is not a valid UUID.
pub(crate) fn declared_default(
    column: &ColumnDef,
    options: &Pg2SqliteOptions,
) -> Result<Option<Expr>, Error> {
    column
        .options
        .iter()
        .find_map(|option| {
            match &option.option {
                ColumnOption::Default(expr) => Some(expr),
                _ => None,
            }
        })
        .map(|expr| {
            let scaled = scaled_default(column, expr)?;
            if is_uuid_data_type(&column.data_type) && is_blob_uuid_representation(options) {
                wrap_uuid_column_default(&column.name, scaled, options)
            } else {
                Ok(scaled)
            }
        })
        .transpose()
}

/// A declared default in the units the translated column stores.
fn scaled_default(column: &ColumnDef, expr: &Expr) -> Result<Expr, Error> {
    let Some(scale) = minor_unit_scale(&column.data_type) else {
        // For NUMERIC(p,0), round fractional literal defaults at translation
        // time.
        return Ok(round_scale_zero_default(&column.data_type, expr));
    };
    scaled_numeric_default(expr, scale)?.ok_or_else(|| {
        Error::forward_refusal(format!(
            "the DEFAULT on column '{}' does not land as one number at the column's scale. The \
             column is a NUMERIC held as an INTEGER of minor units, so the default has to be a \
             plain literal, which PostgreSQL coerces the same way. Write it as a number at the \
             column's scale, or drop it.",
            column.name
        ))
    })
}

/// Rounds a NUMERIC(p,0) column's DEFAULT to an integer when it is a fractional
/// literal, applying PostgreSQL's half-away-from-zero rule. Non-NUMERIC types
/// and non-literal defaults are returned unchanged.
fn round_scale_zero_default(data_type: &DataType, expr: &Expr) -> Expr {
    let Some(info) = exact_numeric_info(data_type) else { return expr.clone() };
    let Ok((_, scale)) = numeric_precision_and_scale(info) else { return expr.clone() };
    if scale != 0 {
        return expr.clone();
    }
    let mut peeled = expr;
    while let Expr::Nested(inner) = peeled {
        peeled = inner;
    }
    round_literal_to_integer(peeled).unwrap_or_else(|| peeled.clone())
}

/// Rounds a plain or negated number literal to the nearest integer using
/// PostgreSQL's half-away-from-zero rule. Returns `None` when `expr` is not a
/// simple number literal that can be rounded.
fn round_literal_to_integer(expr: &Expr) -> Option<Expr> {
    let (negated, digits) = match expr {
        Expr::Value(ValueWithSpan { value: Value::Number(digits, _), .. }) => (false, digits),
        Expr::UnaryOp { op: UnaryOperator::Minus, expr: inner } => {
            match inner.as_ref() {
                Expr::Value(ValueWithSpan { value: Value::Number(digits, _), .. }) => {
                    (true, digits)
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    if digits.contains(['e', 'E']) {
        return None;
    }
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits.as_str(), ""));
    // If the fractional part is entirely zeros the literal is already integral.
    if fraction.chars().all(|c| c == '0') {
        let result = format!("{}{whole}", if negated { "-" } else { "" });
        return Some(Expr::Value(ValueWithSpan {
            value: Value::Number(result, false),
            span: sqlparser::tokenizer::Span::empty(),
        }));
    }
    // Half-away-from-zero: round up when the first fractional digit is >= 5.
    let round_up = fraction.chars().next().is_some_and(|c| c >= '5');
    let whole_val: i64 = whole.parse().ok()?;
    let rounded = if round_up { whole_val + 1 } else { whole_val };
    let result = if negated { format!("-{rounded}") } else { rounded.to_string() };
    Some(Expr::Value(ValueWithSpan {
        value: Value::Number(result, false),
        span: sqlparser::tokenizer::Span::empty(),
    }))
}

/// Translates a column definition, reporting what its declared type loses.
///
/// `table` is taken rather than derived because a warning naming only the
/// column does not identify it: two tables may both have a `created_at`. Both
/// callers, `CREATE TABLE` and `ALTER TABLE ADD COLUMN`, know the name.
///
/// `primary_key_columns` is the table's primary key as its table constraints
/// declare it, which the column alone cannot see and which decides whether the
/// column is SQLite's rowid alias. `ALTER TABLE ADD COLUMN` passes none, since
/// SQLite cannot add a primary key that way.
///
/// This is a free function rather than a
/// [`Translator`](crate::traits::Translator) impl for the same reason: the
/// trait's signature has nowhere to put the table, and an impl that reported an
/// unqualified column would be the defect this exists to fix.
pub(crate) fn translate_column_def(
    column: &ColumnDef,
    table: &ObjectName,
    primary_key_columns: &[String],
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<ColumnDef, crate::errors::Error> {
    // Both an identity column and a serial ask SQLite to supply values, which
    // it does only through the rowid alias, so both need the translated type
    // and the table's key before anything else is decided.
    let has_identity = column
        .options
        .iter()
        .any(|o| matches!(&o.option, ColumnOption::Generated { generation_expr: None, .. }));
    let is_serial = is_serial_type(&column.data_type);

    if has_identity || is_serial {
        let translated_type = column.data_type.translate_with_warnings(schema, options, emit)?;
        if !is_rowid_alias(&translated_type, column, primary_key_columns) {
            return Err(no_value_source(&column.name, is_serial));
        }
        if !is_serial {
            reject_identity_sequence_options(column)?;
        }
        // The identity clause is dropped; INTEGER PRIMARY KEY auto-assigns as
        // the rowid alias.
        let translated_options = column
            .options
            .iter()
            .filter(|o| !matches!(o.option, ColumnOption::Generated { generation_expr: None, .. }))
            .map(|o| o.translate_with_warnings(schema, options, emit))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        return Ok(ColumnDef {
            name: column.name.clone(),
            data_type: translated_type,
            options: translated_options,
        });
    }

    // D1 makes a scaled NUMERIC column an INTEGER of minor units, and the
    // declared DEFAULT writes into it like any other statement, so it scales
    // here, before translation, while the raw literal is still recognisable.
    let mut translated_options: Vec<ColumnOptionDef> = column
        .options
        .iter()
        .map(|o| {
            let ColumnOption::Default(expr) = &o.option else {
                return o.translate_with_warnings(schema, options, emit);
            };
            let scaled = scaled_default(column, expr)?;
            let default_expr =
                if is_uuid_data_type(&column.data_type) && is_blob_uuid_representation(options) {
                    wrap_uuid_column_default(&column.name, scaled, options)?
                } else {
                    scaled
                };
            ColumnOptionDef { name: o.name.clone(), option: ColumnOption::Default(default_expr) }
                .translate_with_warnings(schema, options, emit)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();

    for bound in declared_bound_checks(column, options)? {
        translated_options.push(ColumnOptionDef {
            name: None,
            option: ColumnOption::Check(CheckConstraint {
                name: None,
                expr: Box::new(bound),
                enforced: None,
                no_inherit: false,
            }),
        });
    }

    report_column_downgrades(column, table, emit);

    Ok(ColumnDef {
        name: column.name.clone(),
        data_type: column.data_type.translate_with_warnings(schema, options, emit)?,
        options: translated_options,
    })
}

/// Every bound PostgreSQL enforces through the column's declared type, as
/// `CHECK` expressions SQLite can enforce for itself.
///
/// SQLite's `INTEGER` is 64 bits and its `TEXT` is unbounded, and it promotes
/// an overflowing integer to `REAL` rather than failing, so without these a
/// replica would hold values PostgreSQL refuses. The UUID bound is here for
/// the same reason it is a `CHECK` rather than a translate-time rewrite: a
/// bound parameter never passes through the literal rewrites.
fn declared_bound_checks(
    column: &ColumnDef,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Vec<Expr>, crate::errors::Error> {
    let mut bounds = Vec::new();
    if is_uuid_data_type(&column.data_type) && is_blob_uuid_representation(options) {
        bounds.push(uuid_blob_length_check_expr(&column.name));
    }
    if let Some(info) = exact_numeric_info(&column.data_type) {
        let (precision, _) = numeric_precision_and_scale(info)?;
        bounds.push(numeric_precision_bound_expr(&column.name, precision));
    }
    if let Some(bound) = integer_range_bound(column) {
        bounds.push(bound);
    }
    if let Some(length) = character_length(&column.data_type)? {
        bounds.push(character_length_bound_expr(&column.name, length));
    }
    if let Some((fixed, Some(n))) = bit_length(&column.data_type) {
        bounds.push(if fixed {
            bit_exact_length_check_expr(&column.name, n)
        } else {
            bit_max_length_check_expr(&column.name, n)
        });
    }
    Ok(bounds)
}

/// The `CHECK` bound a narrower PostgreSQL integer needs.
///
/// SQLite's `INTEGER` is 64 bits, so a `smallint` or an `integer` column
/// would hold values PostgreSQL refuses outright, and a replica that then
/// replayed the row against the server would fail there instead.
fn integer_range_bound(column: &ColumnDef) -> Option<Expr> {
    match column.data_type {
        DataType::SmallInt(_) | DataType::Int2(_) => Some(small_int_range_bound_expr(&column.name)),
        DataType::Int(_) | DataType::Integer(_) | DataType::Int4(_) => {
            Some(regular_int_range_bound_expr(&column.name))
        }
        _ => None,
    }
}

/// Refuses an identity column whose sequence options the rowid cannot honour.
///
/// SQLite assigns rowid values from 1 in steps of 1, so `START WITH`,
/// `INCREMENT BY` and their siblings would be silently ignored and every
/// identifier the column produces would be offset from PostgreSQL's.
fn reject_identity_sequence_options(column: &ColumnDef) -> Result<(), crate::errors::Error> {
    for option in &column.options {
        let ColumnOption::Generated {
            generation_expr: None,
            sequence_options: Some(sequence_options),
            ..
        } = &option.option
        else {
            continue;
        };
        if sequence_options.is_empty() {
            continue;
        }
        return Err(Error::forward_refusal(format!(
            "GENERATED AS IDENTITY ({}) on column {} cannot be translated. SQLite assigns rowid \
             values starting from 1 with an increment of 1 and has no sequence engine, so these \
             options cannot be honoured. Remove them and let the rowid assign values, or manage \
             the counter in the application.",
            sequence_options.iter().map(ToString::to_string).collect::<Vec<_>>().join(" "),
            column.name
        )));
    }
    Ok(())
}

/// True when the translated column will be SQLite's rowid alias, the one place
/// SQLite assigns a value on its own.
///
/// The type must be exactly `INTEGER` and the column must be the whole primary
/// key. Both spellings of the key count, the column's own `PRIMARY KEY` option
/// and a single-column `PRIMARY KEY (n)` table constraint, because SQLite makes
/// an alias of either: measured on 3.46.0, `CREATE TABLE x (n INTEGER, t TEXT,
/// PRIMARY KEY (n)) STRICT` assigns 1 and 2 to two rows that name no value. A
/// composite key is not an alias, so a serial inside one has no value source.
fn is_rowid_alias(
    translated_type: &DataType,
    column: &ColumnDef,
    primary_key_columns: &[String],
) -> bool {
    if !matches!(translated_type, DataType::Integer(None)) {
        return false;
    }
    column.options.iter().any(|o| matches!(o.option, ColumnOption::PrimaryKey(_)))
        || matches!(primary_key_columns, [only] if only.eq_ignore_ascii_case(&column.name.value))
}

/// Reports a column that asks SQLite to supply its values where SQLite cannot.
///
/// PostgreSQL's `SERIAL` is shorthand for `integer NOT NULL DEFAULT
/// nextval('...')`, so it is the same request an identity column makes, and
/// SQLite grants it only through the rowid alias. Left alone, a serial off the
/// key emitted a plain `INTEGER` and every row stored NULL in silence, while
/// the identity spelling and the literal `DEFAULT nextval('...')` both already
/// refused.
fn no_value_source(column: &Ident, is_serial: bool) -> Error {
    let construct = if is_serial { "SERIAL" } else { "GENERATED AS IDENTITY" };
    Error::forward_refusal(format!(
        "{construct} on column '{column}' cannot be expressed in SQLite. It asks the database to \
         supply the value, and SQLite does that only for an INTEGER PRIMARY KEY, which is its \
         rowid alias, so a column that is not the whole primary key has no value source. Make it \
         the table's INTEGER PRIMARY KEY, or declare it INTEGER NOT NULL and supply the value on \
         every insert."
    ))
}

/// Reports what a column's declared type loses on the way to SQLite.
///
/// Declared character length and NUMERIC are excluded: both survive as CHECKs
/// or minor-unit storage (D1).
fn report_column_downgrades(
    column: &ColumnDef,
    table: &ObjectName,
    emit: crate::warnings::WarningSink<'_>,
) {
    let location = format!("{}.{}", last_ident_value_or_display(table), column.name.value);

    // CHAR pads to its declared width and TEXT stores what it is given.
    if matches!(column.data_type, DataType::Char(_) | DataType::Character(_)) {
        emit(crate::warnings::TranslationWarning::LossyDowngrade {
            construct: "CHAR".to_string(),
            from: column.data_type.to_string(),
            to: "TEXT".to_string(),
            location: location.clone(),
            reason: "SQLite stores the value as given, so it is no longer blank padded to \
                     the declared width."
                .to_string(),
        });
    }

    // SQLite has no zone-aware temporal type; values compare as text, not as
    // instants.
    if matches!(
        column.data_type,
        DataType::Timestamp(_, TimezoneInfo::Tz | TimezoneInfo::WithTimeZone)
            | DataType::Time(_, TimezoneInfo::Tz | TimezoneInfo::WithTimeZone)
    ) {
        emit(crate::warnings::TranslationWarning::LossyDowngrade {
            construct: "WITH TIME ZONE".to_string(),
            from: column.data_type.to_string(),
            to: "TEXT".to_string(),
            location: location.clone(),
            reason: "SQLite has no zone-aware temporal type, so the value is stored as text. \
                     Equality and ordering compare text, not instants: two values that name the \
                     same moment in different offsets (+02:00 vs +00:00) compare unequal."
                .to_string(),
        });
    }

    // jsonb normalises key order, whitespace, and duplicate keys on write; TEXT
    // stores verbatim.
    if matches!(column.data_type, DataType::JSONB) {
        emit(crate::warnings::TranslationWarning::LossyDowngrade {
            construct: "JSONB".to_string(),
            from: "JSONB".to_string(),
            to: "TEXT".to_string(),
            location: location.clone(),
            reason: "PostgreSQL normalises key order and removes duplicate keys on write; \
                     the replica stores the value verbatim, so ::text projections and \
                     equality against a normalised literal diverge."
                .to_string(),
        });
    }

    // tsvector parses input into sorted, deduplicated lexemes; TEXT stores the
    // raw string.
    if matches!(column.data_type, DataType::TsVector) {
        emit(crate::warnings::TranslationWarning::LossyDowngrade {
            construct: "TSVECTOR".to_string(),
            from: "TSVECTOR".to_string(),
            to: "TEXT".to_string(),
            location,
            reason: "PostgreSQL parses tsvector input into normalised lexemes on write; \
                     the replica stores the raw string, so ::text projections differ."
                .to_string(),
        });
    }
}
