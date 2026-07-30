//! Implementation of the [`Translator`] trait for the
//! `TableConstraint` type.

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
use sqlparser::ast::{NullsDistinctOption, TableConstraint};

use crate::{
    impls::{
        object_name::{append_suffix, table_has_implicit_public_rls},
        shared_helpers::nulls_not_distinct_not_supported_error,
    },
    options::Pg2SqliteOptions,
    prelude::{TranslationOptions, Translator},
};

impl Translator for TableConstraint {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Option<TableConstraint>;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        match self {
            Self::Check(check_constraint) => {
                match check_constraint.expr.translate(schema, options) {
                    Ok(translated_expr) => {
                        Ok(Some(Self::Check(sqlparser::ast::CheckConstraint {
                            name: check_constraint.name.clone(),
                            expr: Box::new(translated_expr),
                            enforced: check_constraint.enforced,
                        })))
                    }
                    Err(_) if options.should_remove_unsupported_check_constraints() => Ok(None),
                    Err(e) => Err(e),
                }
            }
            Self::ForeignKey(fk_constraint) => {
                let mut updated_fk = fk_constraint.clone();

                if table_has_implicit_public_rls(schema, &fk_constraint.foreign_table)? {
                    updated_fk.foreign_table =
                        append_suffix(&fk_constraint.foreign_table, options.get_rls_table_suffix());
                }

                updated_fk.on_delete =
                    fk_constraint.on_delete.map(|a| a.translate(schema, options)).transpose()?;
                updated_fk.on_update =
                    fk_constraint.on_update.map(|a| a.translate(schema, options)).transpose()?;

                updated_fk.characteristics = fk_constraint
                    .characteristics
                    .map(|c| c.translate(schema, options))
                    .transpose()?;

                Ok(Some(Self::ForeignKey(updated_fk)))
            }
            Self::PrimaryKey(pk_constraint) => {
                let mut updated_pk = pk_constraint.clone();
                updated_pk.columns = pk_constraint
                    .columns
                    .iter()
                    .map(|col| col.translate(schema, options))
                    .collect::<Result<Vec<_>, _>>()?;
                updated_pk.characteristics = pk_constraint
                    .characteristics
                    .map(|c| c.translate(schema, options))
                    .transpose()?;
                Ok(Some(Self::PrimaryKey(updated_pk)))
            }
            Self::Unique(unique_constraint) => {
                if matches!(unique_constraint.nulls_distinct, NullsDistinctOption::NotDistinct) {
                    return Err(nulls_not_distinct_not_supported_error());
                }

                let mut updated_unique = unique_constraint.clone();
                updated_unique.columns = unique_constraint
                    .columns
                    .iter()
                    .map(|col| col.translate(schema, options))
                    .collect::<Result<Vec<_>, _>>()?;
                updated_unique.characteristics = unique_constraint
                    .characteristics
                    .map(|c| c.translate(schema, options))
                    .transpose()?;
                // `NULLS DISTINCT` is the default and is what SQLite does, so
                // the clause is dropped rather than emitted: SQLite rejects it
                // with `near "NULLS": syntax error`.
                updated_unique.nulls_distinct = NullsDistinctOption::None;
                Ok(Some(Self::Unique(updated_unique)))
            }
            // Outcome 2 of the reporting policy in `statement.rs`. The
            // passthrough that used to stand here emitted every one of these
            // verbatim, and none has a SQLite form, so each produced SQL that
            // could not run. There is no wildcard arm on purpose: a new
            // `sqlparser` variant fails to compile until it is classified.
            Self::Exclude(exclude) => {
                Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "{exclude} cannot be translated. An exclusion constraint enforces that no two \
                     rows satisfy a comparison, which SQLite has no constraint for. Express it \
                     with a trigger that raises, or with a unique index where the comparison is \
                     equality."
                )))
            }
            Self::Index(index) => {
                Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "{index} cannot be translated. SQLite declares an index with a CREATE INDEX \
                     statement of its own rather than inside the table body."
                )))
            }
            Self::FulltextOrSpatial(constraint) => {
                Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "{constraint} cannot be translated. SQLite offers full-text search through the \
                     FTS5 virtual table rather than a key on an ordinary table."
                )))
            }
            Self::PrimaryKeyUsingIndex(constraint) | Self::UniqueUsingIndex(constraint) => {
                Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "USING INDEX {} cannot be translated. It adopts an existing index as the \
                     constraint, and SQLite has no way to promote an index that way. Declare the \
                     constraint on the table instead.",
                    constraint.index_name
                )))
            }
        }
    }
}
