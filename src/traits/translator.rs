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
use crate::traits::TranslationOptions;

/// Trait to translate between a `PostgreSQL` entry and a `SQLite` entry.
pub trait Translator {
    /// Schema type for the translation.
    type Schema: Schema;
    /// Translation options type.
    type Options: TranslationOptions;
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
