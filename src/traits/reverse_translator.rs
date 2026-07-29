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

/// Trait for reverse translating SQLite DML statements to PostgreSQL,
/// the inverse of [`crate::traits::Translator`].
pub trait ReverseTranslator {
    /// Schema type for the translation.
    type Schema: Schema;
    /// Translation options type.
    type Options: TranslationOptions;
    /// Produced PostgreSQL entry type.
    type PostgresEntry;

    /// Reverse translates a SQLite entry to its PostgreSQL equivalent.
    ///
    /// # Errors
    ///
    /// Returns an error if the reverse translation fails.
    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, crate::errors::Error>;
}
