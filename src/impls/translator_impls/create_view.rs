//! Implementation of the [`Translator`] trait for the
//! `CreateView` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{CreateTableOptions, CreateView};

use crate::{
    errors::Error,
    prelude::{Pg2SqliteOptions, Translator},
};

impl Translator for CreateView {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = CreateView;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // Check for unsupported PostgreSQL features
        if self.materialized {
            return Err(Error::UnsupportedSQLiteFeature(
                "MATERIALIZED VIEW is not supported in SQLite".into(),
            ));
        }

        if self.or_replace {
            return Err(Error::UnsupportedSQLiteFeature(
                "CREATE OR REPLACE VIEW is not supported in SQLite".into(),
            ));
        }

        if self.or_alter {
            return Err(Error::UnsupportedSQLiteFeature(
                "CREATE OR ALTER VIEW is not supported in SQLite".into(),
            ));
        }

        if self.secure {
            return Err(Error::UnsupportedSQLiteFeature(
                "SECURE VIEW is not supported in SQLite".into(),
            ));
        }

        if !matches!(self.options, CreateTableOptions::None) {
            return Err(Error::UnsupportedSQLiteFeature(format!(
                "VIEW options are not supported in SQLite: {:?}",
                self.options
            )));
        }

        if !self.cluster_by.is_empty() {
            return Err(Error::UnsupportedSQLiteFeature(
                "CLUSTER BY is not supported in SQLite views".into(),
            ));
        }

        if self.to.is_some() {
            return Err(Error::UnsupportedSQLiteFeature(
                "VIEW TO clause is not supported in SQLite".into(),
            ));
        }

        if self.with_no_schema_binding {
            return Err(Error::UnsupportedSQLiteFeature(
                "WITH NO SCHEMA BINDING is not supported in SQLite".into(),
            ));
        }

        // Pass through supported properties
        Ok(CreateView {
            or_alter: false,
            or_replace: false,
            materialized: false,
            secure: false,
            name: self.name.clone(),
            name_before_not_exists: self.name_before_not_exists,
            columns: self.columns.clone(),
            query: Box::new(self.query.translate(schema, options)?),
            options: CreateTableOptions::default(),
            cluster_by: Vec::new(),
            comment: self.comment.clone(), // Comments are harmless, pass through
            if_not_exists: self.if_not_exists,
            temporary: self.temporary,
            to: None,
            params: None,
            with_no_schema_binding: false,
        })
    }
}
