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
use crate::{
    errors::Error,
    impls::shared_helpers::translate_update,
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

impl ReverseTranslator for Update {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Update;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        // PostgreSQL UPDATE has no conflict-resolution clause at all. Each SQLite
        // OR mode changes error handling in a distinct way (ROLLBACK reverts the
        // transaction, ABORT stops the statement but keeps prior changes, FAIL
        // stops at the first conflicting row, IGNORE skips conflicting rows,
        // REPLACE deletes and re-inserts), so silently dropping the clause would
        // change the observable error behaviour. Reject all of them.
        if let Some(or_clause) = self.or {
            return Err(Error::UnsupportedSQLiteFeature(format!(
                "UPDATE {or_clause} has no PostgreSQL form. PostgreSQL UPDATE has no \
                 conflict-resolution clause. Use an explicit transaction with appropriate \
                 error handling instead."
            )));
        }
        translate_update::<Reverse>(self, schema, options)
    }
}
