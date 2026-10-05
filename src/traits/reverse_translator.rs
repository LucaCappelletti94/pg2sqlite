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

/// Trait for reverse translating SQLite DML statements to PostgreSQL,
/// the inverse of [`crate::traits::Translator`].
pub trait ReverseTranslator {
    /// Schema type for the translation.
    type Schema: Schema;
    /// Produced PostgreSQL entry type.
    type PostgresEntry;

    /// Reverse translates a SQLite entry to its PostgreSQL equivalent.
    ///
    /// The context carries the settings and, where one is in scope, the
    /// relations a column reference in this entry can name, which is what lets
    /// a type-dependent reversal read the column it actually names.
    ///
    /// # Errors
    ///
    /// Returns an error if the reverse translation fails.
    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<Self::PostgresEntry, crate::errors::Error>;
}
