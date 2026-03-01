//! Shared helper functions for forward translation of table references,
//! joins, and select items.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    Expr, Fetch, LimitClause, OrderBy, PipeOperator, Query, Setting, TableWithJoins, WindowType,
    With,
};

use crate::{
    errors::Error,
    impls::{
        direction_wrappers::define_direction_wrappers,
        object_name::normalize_implicit_public_object_name, shared_helpers::TranslationDirection,
    },
    prelude::{Pg2SqliteOptions, Translator},
};

/// Forward (PostgreSQL → SQLite) translation direction.
pub(crate) struct Forward;

impl TranslationDirection for Forward {
    const IS_FORWARD: bool = true;

    fn translate_expr(
        expr: &Expr,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Expr, Error> {
        expr.translate(schema, options)
    }

    fn translate_query(
        query: &Query,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Query, Error> {
        query.translate(schema, options)
    }

    fn translate_insert(
        insert: &sqlparser::ast::Insert,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<sqlparser::ast::Insert, Error> {
        insert.translate(schema, options)
    }

    fn translate_delete(
        delete: &sqlparser::ast::Delete,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<sqlparser::ast::Delete, Error> {
        match delete.translate(schema, options)? {
            sqlparser::ast::Statement::Delete(d) => Ok(d),
            _ => Ok(delete.clone()),
        }
    }

    fn translate_object_name(
        name: &sqlparser::ast::ObjectName,
        _schema: &ParserDB,
        _options: &Pg2SqliteOptions,
    ) -> Result<sqlparser::ast::ObjectName, Error> {
        normalize_implicit_public_object_name(name)
    }
}

define_direction_wrappers! {
    direction = Forward;
    fn translate_table_with_joins(table_with_joins: &TableWithJoins) -> TableWithJoins = translate_table_with_joins;
    fn translate_query_settings(settings: Option<&Vec<Setting>>) -> Option<Vec<Setting>> = translate_query_settings;
    fn translate_pipe_operators(pipe_operators: &[PipeOperator]) -> Vec<PipeOperator> = translate_pipe_operators;
    fn translate_with_clause(with: Option<&With>) -> Option<With> = translate_with_clause;
    fn translate_order_by_clause(order_by: Option<&OrderBy>) -> Option<OrderBy> = translate_order_by_clause;
    fn translate_limit_clause(limit_clause: Option<&LimitClause>) -> Option<LimitClause> = translate_limit_clause;
    fn translate_fetch_clause(fetch: Option<&Fetch>) -> Option<Fetch> = translate_fetch_clause;
    fn translate_window_type(over: Option<&WindowType>) -> Option<WindowType> = translate_window_type;
}
