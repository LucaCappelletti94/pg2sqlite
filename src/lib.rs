#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod errors;
pub(crate) mod impls;
pub mod manifest;
pub mod options;
pub mod pg2sqlite;
pub mod traits;
pub mod warnings;

/// Prelude module for the library.
pub mod prelude {
    pub use crate::{
        errors::{Error, RefusalCategory, SqlParseError, TranslationDirection, TranslationRefusal},
        manifest::{ColumnManifestEntry, TableManifestEntry, WrapperKind},
        options::Pg2SqliteOptions,
        pg2sqlite::Pg2Sqlite,
        traits::{
            ArrayRepresentation, ReverseTranslator, Schema, SessionVariableMapping,
            SessionVariablePattern, Translator, UuidRepresentation, UuidVersion,
        },
        warnings::{TranslationReport, TranslationWarning},
    };
}
