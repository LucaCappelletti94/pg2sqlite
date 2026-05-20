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
use sqlparser::ast::TableConstraint;

use crate::{
    impls::object_name::{append_suffix, table_has_implicit_public_rls},
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
                Ok(Some(Self::Unique(updated_unique)))
            }
            other => Ok(Some(other.clone())),
        }
    }
}
