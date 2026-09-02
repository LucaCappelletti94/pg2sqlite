//! Submodule with the implementations of the [`crate::traits::Translator`]
//! trait.

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

pub(crate) mod array;
pub(crate) mod column;
pub(crate) mod column_option;
mod condition_injection;
mod constraint_characteristic;
pub(crate) mod create_index;
mod create_table;
mod create_trigger;
mod create_view;
pub(crate) mod data_type;
pub(crate) mod delete;
pub(crate) mod expr;
pub(crate) mod function;
mod helpers;
pub(crate) use helpers::Forward;
mod index_column;
pub(crate) mod insert;
mod order_by_expr;
pub mod plpgsql;
pub mod postgis;
pub(crate) mod query;
mod referential_action;
pub mod rls;
mod statement;
mod table_constraint;
pub(crate) mod update;
pub(crate) mod uuid;
pub mod vector;
