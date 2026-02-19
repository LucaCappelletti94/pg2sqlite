//! Implementation of the [`Translator`] trait for the
//! `Column` type.

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike},
};
use sqlparser::ast::{ColumnOption, ColumnOptionDef, Expr, ForeignKeyConstraint, GeneratedAs};

use crate::{
    impls::object_name::{append_suffix, schema_and_table_for_lookup},
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
                        if func.name.0.first().and_then(|s| Some(s.as_ident()?.value.as_str()))
                            == Some("CURRENT_TIMESTAMP")
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
                        Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                            "Unsupported default expression function: {func}"
                        )))
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
                    // Handle unary operators (e.g., DEFAULT -1)
                    Expr::UnaryOp { op, expr } => {
                        let translated_inner = expr.translate(schema, options)?;
                        Ok(Some(ColumnOptionDef {
                            name: self.name.clone(),
                            option: ColumnOption::Default(Expr::UnaryOp {
                                op: *op,
                                expr: Box::new(translated_inner),
                            }),
                        }))
                    }
                    // Handle nested/parenthesized expressions
                    Expr::Nested(inner) => {
                        let translated_inner = inner.translate(schema, options)?;
                        Ok(Some(ColumnOptionDef {
                            name: self.name.clone(),
                            option: ColumnOption::Default(Expr::Nested(Box::new(translated_inner))),
                        }))
                    }
                    // Handle binary operations (e.g., DEFAULT 1 + 2)
                    Expr::BinaryOp { left, op, right } => {
                        let translated_left = left.translate(schema, options)?;
                        let translated_right = right.translate(schema, options)?;
                        Ok(Some(ColumnOptionDef {
                            name: self.name.clone(),
                            option: ColumnOption::Default(Expr::BinaryOp {
                                left: Box::new(translated_left),
                                op: op.clone(),
                                right: Box::new(translated_right),
                            }),
                        }))
                    }
                    // Handle type casts (e.g., DEFAULT value::type)
                    Expr::Cast { expr, data_type, format, kind, array } => {
                        let translated_expr = expr.translate(schema, options)?;
                        let translated_type = data_type.translate(schema, options)?;
                        Ok(Some(ColumnOptionDef {
                            name: self.name.clone(),
                            option: ColumnOption::Default(Expr::Cast {
                                expr: Box::new(translated_expr),
                                data_type: translated_type,
                                format: format.clone(),
                                kind: kind.clone(),
                                array: *array,
                            }),
                        }))
                    }
                    other => {
                        Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                            "Unsupported default expression: {other}"
                        )))
                    }
                }
            }
            ColumnOption::NotNull | ColumnOption::PrimaryKey(_) => Ok(Some(self.clone())),
            ColumnOption::Check(_) => Ok(None),
            // Generated columns: GENERATED ALWAYS AS (expr) [STORED | VIRTUAL]
            // SQLite supports this syntax since version 3.31.0
            ColumnOption::Generated {
                generated_as,
                sequence_options: _,
                generation_expr,
                generation_expr_mode,
                generated_keyword,
            } => {
                // SQLite only supports ALWAYS or expression-stored, not BY DEFAULT
                if *generated_as == GeneratedAs::ByDefault {
                    return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                        "SQLite only supports GENERATED ALWAYS, not GENERATED BY DEFAULT"
                            .to_string(),
                    ));
                }

                // Translate the generation expression
                let translated_expr =
                    generation_expr.as_ref().map(|e| e.translate(schema, options)).transpose()?;

                // SQLite supports both STORED and VIRTUAL modes
                // VIRTUAL is the default in SQLite if not specified
                Ok(Some(ColumnOptionDef {
                    name: self.name.clone(),
                    option: ColumnOption::Generated {
                        generated_as: *generated_as,
                        sequence_options: None,
                        generation_expr: translated_expr,
                        generation_expr_mode: *generation_expr_mode,
                        generated_keyword: *generated_keyword,
                    },
                }))
            }
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
                // Check if the referenced table has RLS and update the foreign_table reference
                let updated_foreign_table = {
                    let (fk_schema, fk_table_name) = schema_and_table_for_lookup(foreign_table);
                    if let Some(fk_table_name) = fk_table_name
                        && schema
                            .table(fk_schema, fk_table_name)
                            .is_some_and(|table| table.has_row_level_security(schema))
                    {
                        append_suffix(foreign_table, options.get_rls_table_suffix())
                    } else {
                        foreign_table.clone()
                    }
                };

                Ok(Some(Self {
                    name: self.name.clone(),
                    option: ForeignKeyConstraint {
                        name: name.clone(),
                        index_name: index_name.clone(),
                        columns: columns.clone(),
                        match_kind: *match_kind,
                        foreign_table: updated_foreign_table,
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
            other => {
                Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                    "Unsupported column option: {other}"
                )))
            }
        }
    }
}
