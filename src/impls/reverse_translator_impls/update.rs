//! Implementation of the [`ReverseTranslator`] trait for the
//! `Update` type.

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
use sqlparser::ast::Update;

use super::helpers::Reverse;
use crate::{
    errors::Error,
    impls::shared_helpers::translate_update,
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

impl ReverseTranslator for Update {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Update;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        translate_update::<Reverse>(self, schema, options)
    }
}
