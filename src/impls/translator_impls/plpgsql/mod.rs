//! Translates PL/pgSQL trigger function bodies to SQLite-compatible statements.
//!
//! Variables (`variable := expr`) become `WITH var(val) AS (SELECT expr)` CTEs,
//! and IF conditions become WHERE clauses injected into each enclosed DML
//! statement. UUID variables are regenerated per INSERT via the
//! `last_insert_rowid()` pattern.
//!
//! Scanning, context tracking, preprocessing, and body parsing live in
//! `sqlparser-plpgsql` and are re-exported here.

pub use sqlparser_plpgsql::{PlPgSqlContext, VariableBinding, parse_body};

mod cte_builder;
pub(crate) use cte_builder::VARIABLE_VALUE_COLUMN;
mod translator;

pub use translator::PlPgSqlTranslator;
