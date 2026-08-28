//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `CreateView` type.

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
};

crate::traits::translator::impl_contextual_translator!(CreateView => CreateView);
impl crate::traits::translator::TranslatorWithContext for CreateView {
    fn translate_with_warnings(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        if self.materialized {
            return Err(Error::forward_refusal("MATERIALIZED VIEW is not supported in SQLite"));
        }

        if self.or_alter {
            return Err(Error::forward_refusal("CREATE OR ALTER VIEW is not supported in SQLite"));
        }

        if self.secure {
            return Err(Error::forward_refusal("SECURE VIEW is not supported in SQLite"));
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
            return Err(Error::forward_refusal(format!(
                "VIEW options are not supported in SQLite: {:?}",
                self.options
            )));
        }

        if !self.cluster_by.is_empty() {
            return Err(Error::forward_refusal("CLUSTER BY is not supported in SQLite views"));
        }

        if self.to.is_some() {
            return Err(Error::forward_refusal("VIEW TO clause is not supported in SQLite"));
        }

        if self.with_no_schema_binding {
            return Err(Error::forward_refusal(
                "WITH NO SCHEMA BINDING is not supported in SQLite",
            ));
        }

        let mut query = self.query.translate_with_warnings(schema, options, emit)?;
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
    options: &crate::options::TranslationContext<'_>,
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
