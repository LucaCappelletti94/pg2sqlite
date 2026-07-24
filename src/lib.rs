#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod errors;
pub mod impls;
pub mod manifest;
pub mod options;
pub mod pg2sqlite;
pub mod traits;
pub mod warnings;

/// Prelude module for the library.
pub mod prelude {
    pub use crate::{
        manifest::{TableManifestEntry, WrapperKind},
        options::Pg2SqliteOptions,
        pg2sqlite::Pg2Sqlite,
        traits::{
            ReverseTranslator, Schema, SessionVariableMapping, SessionVariablePattern,
            TranslationOptions, Translator, UuidRepresentation, UuidVersion,
        },
        warnings::{TranslationReport, TranslationWarning},
    };
}
