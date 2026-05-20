//! Implementation of the [`Translator`] trait for the
//! `IndexColumn` type.

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
use sqlparser::ast::IndexColumn;

use crate::prelude::{Pg2SqliteOptions, Translator};

impl Translator for IndexColumn {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Self;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        Ok(IndexColumn { column: self.column.translate(schema, options)?, ..self.clone() })
    }
}
