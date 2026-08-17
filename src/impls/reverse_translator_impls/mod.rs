//! Submodule with the implementations of the
//! [`crate::traits::ReverseTranslator`] trait for reverse translating SQLite
//! DML statements to PostgreSQL.

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

mod delete;
mod expr;
pub(crate) mod function;
mod helpers;
mod ident_quoting;
mod insert;
mod query;
mod statement;
mod update;
