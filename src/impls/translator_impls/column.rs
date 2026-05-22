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
use sqlparser::ast::{CheckConstraint, ColumnDef, ColumnOption, ColumnOptionDef};

use crate::{
    impls::translator_impls::uuid::{
        is_blob_uuid_representation, is_uuid_data_type, uuid_blob_length_check_expr,
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

        Ok(ColumnDef {
            name: self.name.clone(),
            data_type: self.data_type.translate(schema, options)?,
            options: translated_options,
        })
    }
}
