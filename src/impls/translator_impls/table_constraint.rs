//! Implementation of the [`Translator`] trait for the
//! `TableConstraint` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Function, TableConstraint};

use crate::{
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
                use crate::impls::translator_impls::rls::table_has_rls;
                let fk_table_name = fk_constraint.foreign_table.to_string();
                let mut updated_fk = fk_constraint.clone();

                if table_has_rls(&fk_table_name, schema) {
                    let suffix = options.get_rls_table_suffix();
                    let new_fk_name = format!("{fk_table_name}{suffix}");

                    // Create new ObjectName with the _rls suffix
                    updated_fk.foreign_table =
                        sqlparser::ast::ObjectName::from(vec![sqlparser::ast::Ident::new(
                            new_fk_name,
                        )]);
                }

                Ok(Some(Self::ForeignKey(updated_fk)))
            }
            other => Ok(Some(other.clone())),
        }
    }
}
