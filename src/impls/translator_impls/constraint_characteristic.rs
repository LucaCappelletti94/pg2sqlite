//! Implementation of the [`Translator`] trait for the
//! `ConstraintCharacteristics` type.

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
use sqlparser::ast::ConstraintCharacteristics;

use crate::prelude::{Pg2SqliteOptions, Translator};

/// Translates the characteristics of a FOREIGN KEY constraint.
///
/// SQLite honours deferred foreign keys, so `DEFERRABLE` and `INITIALLY` pass
/// through. It has no `ENFORCED` clause, and it carries deferrability nowhere
/// but a foreign key clause, so the `PRIMARY KEY` and `UNIQUE` call sites
/// refuse before reaching here. See [`deferrability_outside_a_foreign_key`].
impl Translator for ConstraintCharacteristics {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = ConstraintCharacteristics;

    fn translate(
        &self,
        _schema: &Self::Schema,
        _options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        if self.enforced.is_some() {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "{self} cannot be translated. SQLite has no ENFORCED clause and enforces every \
                 constraint it accepts, so the clause has no form to take."
            )));
        }

        Ok(ConstraintCharacteristics {
            // PostgreSQL reads a bare INITIALLY as DEFERRABLE, verified as
            // `condeferrable=true` in `pg_constraint`, while SQLite answers
            // `near "INITIALLY": syntax error` without the keyword.
            deferrable: self.deferrable.or(self.initially.map(|_| true)),
            initially: self.initially,
            enforced: None,
        })
    }
}

/// Reports deferrability on a constraint that is not a foreign key.
///
/// SQLite's grammar carries `DEFERRABLE` and `INITIALLY` only on a foreign key
/// clause. On a `PRIMARY KEY`, `UNIQUE`, or `CHECK` constraint it answers
/// `near "DEFERRABLE": syntax error`, so there is nothing to emit.
pub(crate) fn deferrability_outside_a_foreign_key(
    constraint: &str,
    characteristics: ConstraintCharacteristics,
) -> crate::errors::Error {
    crate::errors::Error::UnsupportedSQLiteFeature(format!(
        "{characteristics} on a {constraint} constraint cannot be translated. SQLite carries \
         DEFERRABLE and INITIALLY only on a foreign key clause. Move the deferral to the \
         foreign key that needs it, or drop it."
    ))
}
