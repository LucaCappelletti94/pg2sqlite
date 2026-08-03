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
    CheckConstraint, ColumnDef, ColumnOption, ColumnOptionDef, DataType, ObjectName, TimezoneInfo,
};

use crate::{
    errors::Error,
    impls::{
        object_name::last_ident_value_or_display,
        translator_impls::{
            data_type::{
                character_length, character_length_bound_expr, numeric_precision_and_scale,
                numeric_precision_bound_expr,
            },
            uuid::{is_blob_uuid_representation, is_uuid_data_type, uuid_blob_length_check_expr},
        },
    },
    prelude::{Pg2SqliteOptions, Translator},
};

/// Translates a column definition, reporting what its declared type loses.
///
/// `table` is taken rather than derived because a warning naming only the
/// column does not identify it: two tables may both have a `created_at`. Both
/// callers, `CREATE TABLE` and `ALTER TABLE ADD COLUMN`, know the name.
///
/// This is a free function rather than a [`Translator`] impl for the same
/// reason: the trait's signature has nowhere to put the table, and an impl
/// that reported an unqualified column would be the defect this exists to fix.
pub(crate) fn translate_column_def(
    column: &ColumnDef,
    table: &ObjectName,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<ColumnDef, crate::errors::Error> {
    // GENERATED AS IDENTITY (identity columns) must be handled here because we
    // need to know both the data type and whether the column is a PRIMARY KEY,
    // information that is only available at the ColumnDef level.
    let has_identity = column
        .options
        .iter()
        .any(|o| matches!(&o.option, ColumnOption::Generated { generation_expr: None, .. }));

    if has_identity {
        let translated_type = column.data_type.translate(schema, options)?;
        let is_integer_pk = matches!(translated_type, DataType::Integer(None))
            && column.options.iter().any(|o| matches!(o.option, ColumnOption::PrimaryKey(_)));

        if is_integer_pk {
            // INTEGER PRIMARY KEY is a rowid alias in SQLite and already auto-assigns.
            // Drop the identity clause entirely, which is exactly how SERIAL translates.
            let translated_options = column
                .options
                .iter()
                .filter(|o| {
                    !matches!(o.option, ColumnOption::Generated { generation_expr: None, .. })
                })
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

        return Err(Error::UnsupportedSQLiteFeature(format!(
            "GENERATED AS IDENTITY on column '{}' cannot be expressed in SQLite. \
             Only INTEGER PRIMARY KEY columns are rowid aliases that auto-assign. \
             Use an INTEGER PRIMARY KEY column or manage sequencing in the application.",
            column.name
        )));
    }

    let mut translated_options: Vec<ColumnOptionDef> = column
        .options
        .iter()
        .map(|o| o.translate(schema, options))
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
    if let DataType::Numeric(info) | DataType::Decimal(info) = &column.data_type {
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
