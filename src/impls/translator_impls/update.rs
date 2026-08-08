//! Implementation of the [`Translator`] trait for the
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
use sqlparser::ast::{Update, UpdateTableFromKind};

use super::helpers::Forward;
use crate::{
    errors::Error,
    impls::{returning_scope::scope_returning_to_target, shared_helpers::translate_update},
    prelude::{Pg2SqliteOptions, Translator},
};

impl Translator for Update {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Update;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, Error> {
        let mut update = translate_update::<Forward>(self, schema, options)?;
        let auxiliary = update.from.as_ref().map_or(&[][..], |from| {
            let (UpdateTableFromKind::BeforeSet(tables) | UpdateTableFromKind::AfterSet(tables)) =
                from;
            tables.as_slice()
        });
        // SQLite supports UPDATE ... FROM outright, so nothing here is folded
        // away, and the returned list still cannot see those relations.
        let returning = scope_returning_to_target(
            update.returning.take(),
            Some(&update.table.relation),
            auxiliary,
            schema,
            "FROM",
        )?;
        update.returning = returning;
        Ok(update)
    }
}
