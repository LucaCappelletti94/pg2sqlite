//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `ConstraintCharacteristics` type.

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

use sqlparser::ast::ConstraintCharacteristics;

/// Translates the characteristics of a FOREIGN KEY constraint.
///
/// SQLite honours deferred foreign keys, so `DEFERRABLE` and `INITIALLY` pass
/// through. It has no `ENFORCED` clause, and it carries deferrability nowhere
/// but a foreign key clause, so the `PRIMARY KEY` and `UNIQUE` call sites
/// refuse before reaching here, through `deferrability_outside_a_foreign_key`.
impl crate::traits::translator::TranslatorWithContext for ConstraintCharacteristics {
    type SQLiteEntry = ConstraintCharacteristics;

    fn translate_with_warnings(
        &self,
        _schema: &sql_traits::structs::ParserDB,
        _options: &crate::options::TranslationContext<'_>,
        _emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        if self.enforced.is_some() {
            return Err(crate::errors::Error::forward_refusal(format!(
                "{self} cannot be translated. ENFORCED is a MySQL clause that PostgreSQL 17 does \
                 not accept, answering `syntax error at or near \"ENFORCED\"`, so input carrying \
                 it is not the PostgreSQL this crate translates. SQLite has no such clause \
                 either, and enforces every constraint it accepts."
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
/// PostgreSQL 17 accepts the clause and means it: measured over a `UNIQUE
/// DEFERRABLE INITIALLY DEFERRED` column, two rows holding the same value
/// coexist inside one transaction and the `COMMIT` answers `duplicate key
/// value violates unique constraint`. SQLite's grammar carries `DEFERRABLE`
/// and `INITIALLY` only on a foreign key clause, where it defers for real,
/// and on a `PRIMARY KEY`, `UNIQUE` or `CHECK` constraint it answers `near
/// "DEFERRABLE": syntax error`.
///
/// So the clause is refused rather than dropped: dropping it moves the check
/// from the commit to the statement, and a transaction PostgreSQL accepts,
/// one that holds a duplicate only in the middle of its work, would then be
/// refused.
pub(crate) fn deferrability_outside_a_foreign_key(
    constraint: &str,
    characteristics: ConstraintCharacteristics,
) -> crate::errors::Error {
    crate::errors::Error::forward_refusal(format!(
        "{characteristics} on a {constraint} constraint cannot be translated. PostgreSQL checks \
         such a constraint at the commit, and SQLite carries DEFERRABLE and INITIALLY only on a \
         foreign key clause, so the replica would check it at the statement instead: a \
         transaction that holds a duplicate in the middle of its work is accepted by the server \
         and would be refused here. Drop the deferral only if every statement inside a \
         transaction can satisfy the constraint on its own, or move the work the deferral covers \
         into one statement."
    ))
}
