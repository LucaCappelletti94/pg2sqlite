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

/// Translates a PostgreSQL entry with an explicit translation context, which
/// carries the per-statement settings and the sink lossy steps are reported
/// to.
///
/// Implementing this is how a type becomes a [`Translator`]: the blanket impl
/// below supplies [`Translator::translate`] by opening a context over the
/// options and discarding the warnings. There is no second way to get one,
/// which is the point, since the macro this replaced had to be invoked once
/// per type and a forgotten invocation still compiled.
pub trait TranslatorWithContext {
    /// Produced SQLite entry type.
    type SQLiteEntry;

    /// Translates a PostgreSQL entry, reporting lossy steps through `emit`.
    ///
    /// # Errors
    ///
    /// Returns an error if the translation fails.
    fn translate_with_warnings(
        &self,
        schema: &sql_traits::structs::ParserDB,
        context: &TranslationContext<'_>,
        emit: WarningSink<'_>,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error>;
}

impl<T: TranslatorWithContext> Translator for T {
    type Schema = sql_traits::structs::ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = <T as TranslatorWithContext>::SQLiteEntry;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        let context = TranslationContext::new(options);
        <Self as TranslatorWithContext>::translate_with_warnings(
            self,
            schema,
            &context,
            &mut |_| {},
        )
    }
}
