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
use sqlparser::ast::{CheckConstraint, ColumnDef, ColumnOption, ColumnOptionDef, DataType};

use crate::{
    errors::Error,
    impls::translator_impls::{
        data_type::{numeric_precision_and_scale, numeric_precision_bound_expr},
        uuid::{is_blob_uuid_representation, is_uuid_data_type, uuid_blob_length_check_expr},
    },
    prelude::{Pg2SqliteOptions, Translator},
};

impl Translator for ColumnDef {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = ColumnDef;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // GENERATED AS IDENTITY (identity columns) must be handled here because we
        // need to know both the data type and whether the column is a PRIMARY KEY,
        // information that is only available at the ColumnDef level.
        let has_identity = self
            .options
            .iter()
            .any(|o| matches!(&o.option, ColumnOption::Generated { generation_expr: None, .. }));

        if has_identity {
            let translated_type = self.data_type.translate(schema, options)?;
            let is_integer_pk = matches!(translated_type, DataType::Integer(None))
                && self.options.iter().any(|o| matches!(o.option, ColumnOption::PrimaryKey(_)));

            if is_integer_pk {
                // INTEGER PRIMARY KEY is a rowid alias in SQLite and already auto-assigns.
                // Drop the identity clause entirely, which is exactly how SERIAL translates.
                let translated_options = self
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
                    name: self.name.clone(),
                    data_type: translated_type,
                    options: translated_options,
                });
            }

            return Err(Error::UnsupportedSQLiteFeature(format!(
                "GENERATED AS IDENTITY on column '{}' cannot be expressed in SQLite. \
                 Only INTEGER PRIMARY KEY columns are rowid aliases that auto-assign. \
                 Use an INTEGER PRIMARY KEY column or manage sequencing in the application.",
                self.name
            )));
        }

        let mut translated_options: Vec<ColumnOptionDef> = self
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
        if is_uuid_data_type(&self.data_type) && is_blob_uuid_representation(options) {
            translated_options.push(ColumnOptionDef {
                name: None,
                option: ColumnOption::Check(CheckConstraint {
                    name: None,
                    expr: Box::new(uuid_blob_length_check_expr(&self.name)),
                    enforced: None,
                }),
            });
        }

        // SQLite promotes an overflowing integer to REAL with no error, so
        // without this bound an out-of-range value becomes a float.
        if let DataType::Numeric(info) | DataType::Decimal(info) = &self.data_type {
            let (precision, _) = numeric_precision_and_scale(info)?;
            translated_options.push(ColumnOptionDef {
                name: None,
                option: ColumnOption::Check(CheckConstraint {
                    name: None,
                    expr: Box::new(numeric_precision_bound_expr(&self.name, precision)),
                    enforced: None,
                }),
            });
        }

        Ok(ColumnDef {
            name: self.name.clone(),
            data_type: self.data_type.translate(schema, options)?,
            options: translated_options,
        })
    }
}
