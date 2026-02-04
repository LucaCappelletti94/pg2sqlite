//! Implementation of the [`Translator`] trait for the
//! `Column` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{ColumnOption, ColumnOptionDef, Expr, ForeignKeyConstraint};

use crate::{
    prelude::{Pg2SqliteOptions, Translator},
    traits::TranslationOptions,
};

impl Translator for ColumnOptionDef {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Option<ColumnOptionDef>;

    #[allow(clippy::too_many_lines)]
    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        match &self.option {
            ColumnOption::Unique(unique_constraint) => {
                Ok(Some(ColumnOptionDef {
                    name: self.name.clone(),
                    option: unique_constraint.clone().into(),
                }))
            }
            ColumnOption::Default(expr) => {
                match expr {
                    Expr::Function(func) => {
                        if let Some("CURRENT_TIMESTAMP") =
                            func.name.0.first().and_then(|s| Some(s.as_ident()?.value.as_str()))
                        {
                            return Ok(Some(ColumnOptionDef {
                                name: self.name.clone(),
                                option: ColumnOption::Default(Expr::Function(func.clone())),
                            }));
                        }
                        // Translate UUID functions to use the configured function name
                        let func_name =
                            func.name.0.first().and_then(|s| Some(s.as_ident()?.value.as_str()));
                        if let Some(name) = func_name {
                            let is_uuid_func = matches!(
                                name.to_lowercase().as_str(),
                                "gen_random_uuid" | "uuidv4" | "uuidv7"
                            );

                            if is_uuid_func {
                                let mut new_func = func.clone();
                                new_func.name.0 = vec![sqlparser::ast::ObjectNamePart::Identifier(
                                    sqlparser::ast::Ident::new(options.get_uuid_function_name()),
                                )];
                                return Ok(Some(ColumnOptionDef {
                                    name: self.name.clone(),
                                    option: ColumnOption::Default(Expr::Nested(Box::new(
                                        Expr::Function(new_func),
                                    ))),
                                }));
                            }
                        }
                        unimplemented!("The default expression function {func:?} is not supported",)
                    }
                    Expr::Value(value) => {
                        Ok(Some(ColumnOptionDef {
                            name: self.name.clone(),
                            option: ColumnOption::Default(Expr::Value(value.clone())),
                        }))
                    }
                    Expr::Identifier(ident) => {
                        Ok(Some(ColumnOptionDef {
                            name: self.name.clone(),
                            option: ColumnOption::Default(Expr::Identifier(ident.clone())),
                        }))
                    }
                    unimplemented => {
                        unimplemented!(
                            "The default expression {:?} is not supported",
                            unimplemented
                        )
                    }
                }
            }
            ColumnOption::NotNull | ColumnOption::PrimaryKey(_) => Ok(Some(self.clone())),
            ColumnOption::Check(_) => Ok(None),
            ColumnOption::ForeignKey(ForeignKeyConstraint {
                name,
                index_name,
                columns,
                match_kind,
                foreign_table,
                referred_columns,
                on_delete,
                on_update,
                characteristics,
            }) => {
                Ok(Some(Self {
                    name: self.name.clone(),
                    option: ForeignKeyConstraint {
                        name: name.clone(),
                        index_name: index_name.clone(),
                        columns: columns.clone(),
                        match_kind: *match_kind,
                        foreign_table: foreign_table.clone(),
                        referred_columns: referred_columns.clone(),
                        on_delete: on_delete
                            .map(|on_delete| on_delete.translate(schema, options))
                            .transpose()?,
                        on_update: on_update
                            .map(|on_update| on_update.translate(schema, options))
                            .transpose()?,
                        characteristics: characteristics
                            .map(|c| c.translate(schema, options))
                            .transpose()?,
                    }
                    .into(),
                }))
            }
            unimplemented => {
                unimplemented!("The column option {unimplemented:?} is not supported")
            }
        }
    }
}
