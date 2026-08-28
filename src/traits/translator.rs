//! Submodule providing a trait to translate between a `PostgreSQL` entry and a
//! `SQLite` entry.

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

use super::Schema;
use crate::{
    options::{Pg2SqliteOptions, TranslationContext},
    warnings::WarningSink,
};

/// Translates a PostgreSQL entry to its SQLite equivalent.
pub trait Translator {
    /// Schema type for the translation.
    type Schema: Schema;
    /// Translation options type.
    type Options;
    /// Produced SQLite entry type.
    type SQLiteEntry;

    /// Translates a PostgreSQL entry to its SQLite equivalent.
    ///
    /// # Errors
    ///
    /// Returns an error if the translation fails.
    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error>;
}

pub(crate) trait TranslatorWithContext:
    Translator<Schema = sql_traits::structs::ParserDB, Options = Pg2SqliteOptions>
{
    fn translate_with_warnings(
        &self,
        schema: &Self::Schema,
        context: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error>;
}

macro_rules! impl_contextual_translator {
    ($source:ty => $output:ty) => {
        impl crate::traits::translator::Translator for $source {
            type Schema = sql_traits::structs::ParserDB;
            type Options = crate::options::Pg2SqliteOptions;
            type SQLiteEntry = $output;

            fn translate(
                &self,
                schema: &Self::Schema,
                options: &Self::Options,
            ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
                let context = crate::options::TranslationContext::new(options);
                <Self as crate::traits::translator::TranslatorWithContext>::translate_with_warnings(
                    self,
                    schema,
                    &context,
                    &mut |_| {},
                )
            }
        }
    };
}

pub(crate) use impl_contextual_translator;
