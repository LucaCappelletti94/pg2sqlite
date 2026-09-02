//! Implementation of the [`ReverseTranslator`] trait for the
//! `Update` type.

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

use sql_traits::structs::ParserDB;
use sqlparser::ast::Update;

use super::helpers::Reverse;
use crate::{errors::Error, impls::shared_helpers::translate_update, prelude::ReverseTranslator};

impl ReverseTranslator for Update {
    type Schema = ParserDB;
    type PostgresEntry = Update;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
    ) -> Result<Self::PostgresEntry, Error> {
        // PostgreSQL UPDATE has no conflict-resolution clause at all. Each
        // SQLite OR mode changes error handling in a distinct way
        // (ROLLBACK reverts the transaction, ABORT stops the statement
        // but keeps prior changes, FAIL stops at the first conflicting
        // row, IGNORE skips conflicting rows, REPLACE deletes and
        // re-inserts), so silently dropping the clause would change the
        // observable error behaviour. Reject all of them.
        if let Some(or_clause) = self.or {
            return Err(Error::reverse_refusal(format!(
                "UPDATE {or_clause} has no PostgreSQL form. PostgreSQL UPDATE has no \
                 conflict-resolution clause. Use an explicit transaction with appropriate \
                 error handling instead."
            )));
        }
        // PostgreSQL UPDATE has no ORDER BY or LIMIT clause. Refuse them so
        // the emitted SQL does not fail at the server with a syntax error.
        if !self.order_by.is_empty() || self.limit.is_some() {
            return Err(Error::reverse_refusal(
                "PostgreSQL UPDATE has no ORDER BY or LIMIT clause; these are SQLite extensions \
             with no PostgreSQL form"
                    .to_string(),
            ));
        }
        translate_update::<Reverse>(self, schema, options, &mut |_| {})
    }
}
