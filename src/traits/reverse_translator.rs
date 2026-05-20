//! Submodule providing a trait to reverse translate between a `SQLite` entry
//! and a `PostgreSQL` entry.

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
use crate::traits::TranslationOptions;

/// Trait to reverse translate between a `SQLite` entry and a `PostgreSQL`
/// entry.
///
/// This is the inverse of the [`crate::traits::Translator`] trait, converting
/// SQLite DML statements back to their PostgreSQL equivalents.
pub trait ReverseTranslator {
    /// The schema type to be used for the translation.
    type Schema: Schema;
    /// The translation options to be used for the translation.
    type Options: TranslationOptions;
    /// The `PostgreSQL` entry type to be used for the translation.
    type PostgresEntry;

    /// Reverse translate a `SQLite` entry to a `PostgreSQL` entry.
    ///
    /// # Arguments
    ///
    /// * `schema` - The schema to be used for the translation.
    /// * `options` - The translation options to be used for the translation.
    ///
    /// # Errors
    ///
    /// * `crate::errors::Error` - If the reverse translation fails.
    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, crate::errors::Error>;
}
