//! Implementation of the [`Translator`] trait for the
//! `CreateTable` type.

use std::collections::HashSet;

use sql_traits::structs::ParserDB;
use sqlparser::ast::{ColumnOption, ColumnOptionDef, CreateTable, TableConstraint};

use crate::{
    impls::object_name::sqlite_unqualified_object_name,
    prelude::{Pg2SqliteOptions, Translator},
};

impl Translator for CreateTable {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = CreateTable;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // STRICT mode is only valid for regular CREATE TABLE, not CREATE TABLE AS
        // SELECT
        let is_ctas = self.query.is_some();

        let mut created_table = Self {
            name: sqlite_unqualified_object_name(&self.name),
            columns: self
                .columns
                .iter()
                .map(|c| c.translate(schema, options))
                .collect::<Result<Vec<_>, _>>()?,
            constraints: self
                .constraints
                .iter()
                .map(|c| c.translate(schema, options))
                .collect::<Result<Vec<Option<TableConstraint>>, _>>()?
                .into_iter()
                .flatten()
                .collect(),
            // SQLite STRICT mode enforces type checking (not valid on CTAS)
            strict: !is_ctas,
            ..self.clone()
        };

        // Translate CREATE TABLE ... AS SELECT subquery
        if let Some(ref q) = self.query {
            created_table.query = Some(Box::new(q.translate(schema, options)?));
        }

        let mut pk_column_names = HashSet::new();

        for constraint in &created_table.constraints {
            if let TableConstraint::PrimaryKey(pk_constraint) = constraint {
                for col in &pk_constraint.columns {
                    if let sqlparser::ast::Expr::Identifier(ident) = &col.column.expr {
                        pk_column_names.insert(ident.value.clone());
                    }
                }
            }
        }

        for col in &created_table.columns {
            for option in &col.options {
                if let ColumnOption::PrimaryKey(_) = &option.option {
                    pk_column_names.insert(col.name.value.clone());
                }
            }
        }

        for col in &mut created_table.columns {
            if pk_column_names.contains(&col.name.value)
                && !col.options.iter().any(|o| matches!(o.option, ColumnOption::NotNull))
            {
                col.options.push(ColumnOptionDef { name: None, option: ColumnOption::NotNull });
            }
        }

        Ok(created_table)
    }
}
