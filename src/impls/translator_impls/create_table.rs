//! Implementation of the [`Translator`] trait for the
//! `CreateTable` type.

use alloc::collections::BTreeSet;
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
use sqlparser::ast::{ColumnOption, ColumnOptionDef, CreateTable, TableConstraint};

use crate::{
    impls::object_name::normalize_schema_qualified_object_name_for_sqlite,
    prelude::{Pg2SqliteOptions, Translator},
    warnings::{TranslationWarning, emit as emit_warning},
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
        // LIKE t is the most dangerous unsupported clause: SQLite would parse it as
        // a column named LIKE of type t and silently create a table with the wrong
        // schema. Reject before emitting anything.
        if self.like.is_some() {
            let table_name = self.name.to_string();
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "CREATE TABLE {table_name} (LIKE ...) cannot be translated to SQLite. \
                 SQLite would silently accept LIKE as a column name and create a table \
                 with a completely wrong schema. Spell out the columns explicitly instead."
            )));
        }

        // INHERITS has no SQLite equivalent.
        if self.inherits.is_some() {
            let table_name = self.name.to_string();
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "CREATE TABLE {table_name} ... INHERITS (...) cannot be translated to SQLite. \
                 SQLite has no table inheritance. Spell out the inherited columns explicitly."
            )));
        }

        // PARTITION OF has no SQLite equivalent.
        if self.partition_of.is_some() {
            let table_name = self.name.to_string();
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "CREATE TABLE {table_name} PARTITION OF ... cannot be translated to SQLite. \
                 SQLite has no partitioned tables."
            )));
        }

        // UNLOGGED is a durability hint with no SQLite equivalent. Drop it and warn.
        if self.unlogged {
            emit_warning(TranslationWarning::LossyDrop {
                construct: "UNLOGGED",
                reason: "SQLite has no UNLOGGED durability setting so the modifier was dropped \
                         and the table is created as a regular table.",
            });
        }

        // STRICT mode is only valid for regular CREATE TABLE, not CREATE TABLE AS
        // SELECT.
        let is_ctas = self.query.is_some();

        let mut created_table = Self {
            name: normalize_schema_qualified_object_name_for_sqlite(schema, &self.name)?,
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
            // SQLite STRICT mode enforces type checking (not valid on CTAS).
            strict: !is_ctas,
            // Drop the UNLOGGED flag so the emitted SQL is valid SQLite.
            unlogged: false,
            ..self.clone()
        };

        if let Some(ref q) = self.query {
            created_table.query = Some(Box::new(q.translate(schema, options)?));
        }

        let mut pk_column_names = BTreeSet::new();

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
