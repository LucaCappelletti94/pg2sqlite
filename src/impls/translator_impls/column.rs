//! Implementation of the [`Translator`] trait for the
//! `Column` type.

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
    TimezoneInfo, Value, ValueWithSpan,
};

use crate::{
    errors::Error,
    impls::{
        object_name::last_ident_value_or_display,
        shared_helpers::{minor_unit_scale, scale_decimal_literal},
        translator_impls::{
            data_type::{
                character_length, character_length_bound_expr, exact_numeric_info, is_serial_type,
                numeric_precision_and_scale, numeric_precision_bound_expr,
            },
            uuid::{is_blob_uuid_representation, is_uuid_data_type, uuid_blob_length_check_expr},
        },
    },
    prelude::{Pg2SqliteOptions, Translator},
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
/// This is a free function rather than a [`Translator`] impl for the same
/// reason: the trait's signature has nowhere to put the table, and an impl
/// that reported an unqualified column would be the defect this exists to fix.
pub(crate) fn translate_column_def(
    column: &ColumnDef,
    table: &ObjectName,
    primary_key_columns: &[String],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
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
        let translated_type = column.data_type.translate(schema, options)?;
        if !is_rowid_alias(&translated_type, column, primary_key_columns) {
            return Err(no_value_source(&column.name, is_serial));
        }

        // INTEGER PRIMARY KEY is a rowid alias in SQLite and already
        // auto-assigns, so the identity clause is dropped, which is exactly
        // how a serial translates.
        let translated_options = column
            .options
            .iter()
            .filter(|o| !matches!(o.option, ColumnOption::Generated { generation_expr: None, .. }))
            .map(|o| o.translate(schema, options))
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
    let scale = minor_unit_scale(&column.data_type);
    let mut translated_options: Vec<ColumnOptionDef> = column
        .options
        .iter()
        .map(|o| {
            let (Some(scale), ColumnOption::Default(expr)) = (scale, &o.option) else {
                return o.translate(schema, options);
            };
            let Some(scaled) = scaled_numeric_default(expr, scale)? else {
                return Err(Error::UnsupportedSQLiteFeature(format!(
                    "the DEFAULT on column '{}' does not land as one number at the column's \
                     scale. The column is a NUMERIC held as an INTEGER of minor units, so the \
                     default has to be a plain literal, which PostgreSQL coerces the same way. \
                     Write it as a number at the column's scale, or drop it.",
                    column.name
                )));
            };
            ColumnOptionDef { name: o.name.clone(), option: ColumnOption::Default(scaled) }
                .translate(schema, options)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();

    // Belt-and-braces for UUID-Blob columns: a column-level
    // `CHECK (length(<col>) = 16)` so parameterised inserts (which
    // bypass the translate-time text-literal wrap) still get
    // rejected by SQLite when the bound value is not 16 bytes.
    if is_uuid_data_type(&column.data_type) && is_blob_uuid_representation(options) {
        translated_options.push(ColumnOptionDef {
            name: None,
            option: ColumnOption::Check(CheckConstraint {
                name: None,
                expr: Box::new(uuid_blob_length_check_expr(&column.name)),
                enforced: None,
            }),
        });
    }

    // SQLite promotes an overflowing integer to REAL with no error, so
    // without this bound an out-of-range value becomes a float.
    if let Some(info) = exact_numeric_info(&column.data_type) {
        let (precision, _) = numeric_precision_and_scale(info)?;
        translated_options.push(ColumnOptionDef {
            name: None,
            option: ColumnOption::Check(CheckConstraint {
                name: None,
                expr: Box::new(numeric_precision_bound_expr(&column.name, precision)),
                enforced: None,
            }),
        });
    }

    // PostgreSQL refuses a value longer than a declared character length,
    // so the bound travels as a CHECK rather than disappearing into TEXT.
    if let Some(length) = character_length(&column.data_type)? {
        translated_options.push(ColumnOptionDef {
            name: None,
            option: ColumnOption::Check(CheckConstraint {
                name: None,
                expr: Box::new(character_length_bound_expr(&column.name, length)),
                enforced: None,
            }),
        });
    }

    report_column_downgrades(column, table);

    Ok(ColumnDef {
        name: column.name.clone(),
        data_type: column.data_type.translate(schema, options)?,
        options: translated_options,
    })
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
    Error::UnsupportedSQLiteFeature(format!(
        "{construct} on column '{column}' cannot be expressed in SQLite. It asks the database to \
         supply the value, and SQLite does that only for an INTEGER PRIMARY KEY, which is its \
         rowid alias, so a column that is not the whole primary key has no value source. Make it \
         the table's INTEGER PRIMARY KEY, or declare it INTEGER NOT NULL and supply the value on \
         every insert."
    ))
}

/// Reports what a column's declared type loses on the way to SQLite.
///
/// Only losses the emitted schema cannot make good. A declared character
/// length is not here, because it survives as a `CHECK`, and `NUMERIC` is not
/// here because D1 maps it exactly.
fn report_column_downgrades(column: &ColumnDef, table: &ObjectName) {
    let location = format!("{}.{}", last_ident_value_or_display(table), column.name.value);

    // CHAR pads to its declared width and TEXT stores what it is given.
    if matches!(column.data_type, DataType::Char(_) | DataType::Character(_)) {
        crate::warnings::emit(crate::warnings::TranslationWarning::LossyDowngrade {
            construct: "CHAR".to_string(),
            from: column.data_type.to_string(),
            to: "TEXT".to_string(),
            location: location.clone(),
            reason: "SQLite stores the value as given, so it is no longer blank padded to \
                     the declared width."
                .to_string(),
        });
    }

    // SQLite has no zone-aware temporal type. The column becomes TEXT holding
    // whatever offset the writer put in it, and nothing converts or compares
    // it as an instant, so the zone is the caller's to carry from here.
    if matches!(
        column.data_type,
        DataType::Timestamp(_, TimezoneInfo::Tz | TimezoneInfo::WithTimeZone)
            | DataType::Time(_, TimezoneInfo::Tz | TimezoneInfo::WithTimeZone)
    ) {
        crate::warnings::emit(crate::warnings::TranslationWarning::LossyDowngrade {
            construct: "WITH TIME ZONE".to_string(),
            from: column.data_type.to_string(),
            to: "TEXT".to_string(),
            location,
            reason: "SQLite has no zone-aware temporal type, so the value is stored as written \
                     and no longer names an instant on its own."
                .to_string(),
        });
    }
}
