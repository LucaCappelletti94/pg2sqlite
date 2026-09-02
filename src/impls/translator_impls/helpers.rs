//! Shared helper functions for forward translation of table references,
//! joins, and select items.

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
use sqlparser::ast::{
    Expr, OrderBy, PipeOperator, Query, Setting, TableWithJoins, WindowType, With,
};

use crate::{
    errors::Error,
    impls::{
        direction_wrappers::define_direction_wrappers,
        object_name::normalize_schema_qualified_object_name_for_sqlite,
        shared_helpers::TranslationDirection,
    },
    prelude::Pg2SqliteOptions,
    traits::translator::TranslatorWithContext,
};

/// Forward (PostgreSQL → SQLite) translation direction.
pub(crate) struct Forward;

impl TranslationDirection for Forward {
    const IS_FORWARD: bool = true;
    type Options<'a> = crate::options::TranslationContext<'a>;

    fn cte_clause<'options>(
        options: &'options Self::Options<'_>,
    ) -> Option<&'options sqlparser::ast::With> {
        options.cte_clause()
    }

    fn with_scope<'scope>(
        options: &'scope Self::Options<'_>,
        scope: &'scope sql_traits::structs::ColumnScope<
            'scope,
            'scope,
            sql_traits::structs::ParserDB,
        >,
    ) -> Self::Options<'scope> {
        options.with_scope(scope)
    }
    fn config<'options>(options: &'options Self::Options<'_>) -> &'options Pg2SqliteOptions {
        options
    }

    fn forward_context<'options, 'config>(
        options: &'options Self::Options<'config>,
    ) -> Option<&'options crate::options::TranslationContext<'config>> {
        Some(options)
    }

    fn translate_expr(
        expr: &Expr,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Expr, Error> {
        expr.translate_with_warnings(schema, options, emit)
    }

    fn translate_query(
        query: &Query,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Query, Error> {
        query.translate_with_warnings(schema, options, emit)
    }

    fn translate_insert(
        insert: &sqlparser::ast::Insert,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<sqlparser::ast::Insert, Error> {
        insert.translate_with_warnings(schema, options, emit)
    }

    fn translate_delete(
        delete: &sqlparser::ast::Delete,
        schema: &ParserDB,
        options: &crate::options::TranslationContext<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<sqlparser::ast::Delete, Error> {
        match delete.translate_with_warnings(schema, options, emit)? {
            sqlparser::ast::Statement::Delete(d) => Ok(d),
            _ => Ok(delete.clone()),
        }
    }

    fn translate_object_name(
        name: &sqlparser::ast::ObjectName,
        schema: &ParserDB,
        _options: &crate::options::TranslationContext<'_>,
    ) -> Result<sqlparser::ast::ObjectName, Error> {
        normalize_schema_qualified_object_name_for_sqlite(schema, name)
    }
}

define_direction_wrappers! {
    direction = Forward;
    fn translate_table_with_joins(table_with_joins: &TableWithJoins) -> TableWithJoins = translate_table_with_joins;
    fn translate_query_settings(settings: Option<&Vec<Setting>>) -> Option<Vec<Setting>> = translate_query_settings;
    fn translate_pipe_operators(pipe_operators: &[PipeOperator]) -> Vec<PipeOperator> = translate_pipe_operators;
    fn translate_with_clause(with: Option<&With>) -> Option<With> = translate_with_clause;
    fn translate_order_by_clause(order_by: Option<&OrderBy>) -> Option<OrderBy> = translate_order_by_clause;
    fn translate_window_type(over: Option<&WindowType>) -> Option<WindowType> = translate_window_type;
}
