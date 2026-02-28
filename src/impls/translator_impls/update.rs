//! Implementation of the [`Translator`] trait for the
//! `Update` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::Update;

use super::helpers::Forward;
use crate::{
    errors::Error,
    impls::shared_helpers::translate_update,
    prelude::{Pg2SqliteOptions, Translator},
};

impl Translator for Update {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Update;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, Error> {
        translate_update::<Forward>(self, schema, options)
    }
}
