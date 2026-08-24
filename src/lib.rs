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
        errors::Error,
        manifest::{ColumnManifestEntry, TableManifestEntry, WrapperKind},
        options::Pg2SqliteOptions,
        pg2sqlite::Pg2Sqlite,
        traits::{
            ArrayRepresentation, ReverseTranslator, Schema, SessionVariableMapping,
            SessionVariablePattern, TranslationOptions, Translator, UuidRepresentation,
            UuidVersion,
        },
        warnings::{TranslationReport, TranslationWarning},
    };
}

/// White-box access for the crate's own integration tests.
///
/// Everything here is internal machinery re-exported so the test suite can
/// exercise inventories and generators directly. It is not part of the API
/// and may change or vanish in any release.
#[doc(hidden)]
pub mod internals {
    #[cfg(feature = "std")]
    pub use crate::impls::sqlite_functions::{
        gated_math, postgres_only, shared_with_postgres, sqlite_has, sqlite_names,
    };
    pub use crate::impls::translator_impls::{
        postgis,
        rls::{
            generate_readonly_rls_statements, generate_rls_statements,
            generate_rls_validation_statements, generate_rls_view_sql, resolve_trigger_table_name,
            table_has_rls,
        },
    };
}
