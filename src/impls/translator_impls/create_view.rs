//! Implementation of the [`Translator`] trait for the
//! `CreateView` type.

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
use core::ops::ControlFlow;

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    CreateTableOptions, CreateView, Expr, ObjectName, Query, SqlOption, Value, visit_relations_mut,
};

use crate::{
    errors::Error,
    impls::object_name::{
        append_suffix, normalize_schema_qualified_object_name_for_sqlite,
        table_has_implicit_public_rls,
    },
    prelude::{Pg2SqliteOptions, Translator},
    traits::TranslationOptions,
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
        if self.materialized {
            return Err(Error::UnsupportedSQLiteFeature(
                "MATERIALIZED VIEW is not supported in SQLite".into(),
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

        // PostgreSQL runs a view with its owner's rights, so a view over a
        // table with row level security bypasses that policy unless it is
        // declared `security_invoker`. Measured on PostgreSQL 17 over a table
        // of three rows whose policy admits two: a plain view answers 3, a
        // `security_invoker` view answers 2. The clause is therefore honoured
        // by which relation the body reads, below, and is not emitted, since
        // SQLite has no view option to carry it.
        let security_invoker = reads_as_the_invoker(&self.options);
        if !matches!(self.options, CreateTableOptions::None) && !security_invoker {
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

        let mut query = self.query.translate(schema, options)?;
        if !security_invoker {
            retarget_rls_reads(&mut query, schema, options)?;
        }

        Ok(CreateView {
            or_alter: false,
            or_replace: false,
            materialized: false,
            secure: false,
            copy_grants: false,
            name: normalize_schema_qualified_object_name_for_sqlite(schema, &self.name)?,
            name_before_not_exists: self.name_before_not_exists,
            columns: self.columns.clone(),
            query: Box::new(query),
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

/// True when the view declares `security_invoker = true`, so it reads with the
/// caller's rights and its base table's row level security applies to it.
fn reads_as_the_invoker(options: &CreateTableOptions) -> bool {
    let CreateTableOptions::With(pairs) = options else { return false };
    matches!(
        pairs.as_slice(),
        [SqlOption::KeyValue { key, value }]
            if key.value.eq_ignore_ascii_case("security_invoker")
                && matches!(value, Expr::Value(literal) if literal.value == Value::Boolean(true))
    )
}

/// Points the view body at the backing table of every row level security table
/// it reads, which is what makes it bypass the policy the way PostgreSQL does.
///
/// The same retarget a foreign key referencing such a table already gets, so a
/// declared view and a declared reference now agree about which relation holds
/// the rows. Without it the body kept reading the policy view, which both
/// filtered rows PostgreSQL would have shown and made a policy that consults
/// the view through another view circularly defined.
fn retarget_rls_reads(
    query: &mut Query,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    let outcome = visit_relations_mut(query, |name: &mut ObjectName| {
        match table_has_implicit_public_rls(schema, name) {
            Ok(true) => {
                *name = append_suffix(name, options.get_rls_table_suffix());
                ControlFlow::Continue(())
            }
            Ok(false) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(error),
        }
    });
    match outcome {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}
