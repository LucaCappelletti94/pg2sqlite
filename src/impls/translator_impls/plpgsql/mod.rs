//! Translates PL/pgSQL trigger function bodies to SQLite-compatible statements.
//!
//! Variables (`variable := expr`) become `WITH var(val) AS (SELECT expr)` CTEs,
//! and IF conditions become WHERE clauses injected into each enclosed DML
//! statement. UUID variables are regenerated per INSERT via the
//! `last_insert_rowid()` pattern.

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

mod context;
mod cte_builder;
mod preprocessor;
mod translator;

pub use context::PlPgSqlContext;
pub use cte_builder::CteBuilder;
pub use preprocessor::PlPgSqlPreprocessor;
pub use translator::PlPgSqlTranslator;
