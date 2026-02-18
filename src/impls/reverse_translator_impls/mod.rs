//! Submodule with the implementations of the
//! [`crate::traits::ReverseTranslator`] trait for reverse translating SQLite
//! DML statements to PostgreSQL.

mod delete;
mod expr;
mod function;
mod helpers;
mod insert;
mod query;
mod statement;
mod update;
