//! Submodule for traits used in the translation between `PostgreSQL` and
//! `SQLite`.

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

pub mod translator;
pub use translator::Translator;
pub mod reverse_translator;
pub use reverse_translator::ReverseTranslator;
pub mod schema;
pub use schema::Schema;
pub mod translation_options;
pub use translation_options::{
    ArrayRepresentation, SessionVariableMapping, SessionVariablePattern, TranslationOptions,
    UuidRepresentation, UuidVersion,
};
