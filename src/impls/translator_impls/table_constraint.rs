//! Implementation of the [`Translator`] trait for the
//! `TableConstraint` type.

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike},
};
use sqlparser::ast::{Function, TableConstraint};

use crate::{
    impls::object_name::{append_suffix, schema_and_table_for_lookup},
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
                match check_constraint.expr.as_ref() {
                    sqlparser::ast::Expr::Function(Function { name, .. }) => {
                        let function_name = name.to_string();
                        if options.should_remove_unsupported_check_constraints() {
                            Ok(None)
                        } else {
                            Err(crate::errors::Error::UndefinedFunction(function_name))
                        }
                    }
                    _ => Ok(Some(self.clone())),
                }
            }
            Self::ForeignKey(fk_constraint) => {
                // Check if the referenced table has RLS and update the foreign_table reference
                let mut updated_fk = fk_constraint.clone();

                let (fk_schema, fk_table_name) =
                    schema_and_table_for_lookup(&fk_constraint.foreign_table);
                if let Some(fk_table_name) = fk_table_name
                    && schema
                        .table(fk_schema, fk_table_name)
                        .is_some_and(|table| table.has_row_level_security(schema))
                {
                    updated_fk.foreign_table =
                        append_suffix(&fk_constraint.foreign_table, options.get_rls_table_suffix());
                }

                Ok(Some(Self::ForeignKey(updated_fk)))
            }
            other => Ok(Some(other.clone())),
        }
    }
}
