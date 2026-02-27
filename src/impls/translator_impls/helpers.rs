//! Shared helper functions for forward translation of table references,
//! joins, and select items.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    ConnectByKind, Expr, Fetch, GroupByExpr, LimitClause, NamedWindowDefinition, OrderBy,
    OrderByExpr, PipeOperator, Query, SelectItem, Setting, TableWithJoins, Values, WindowType,
    With,
};

use crate::{
    errors::Error,
    impls::{
        direction_wrappers::define_direction_wrappers, object_name::sqlite_unqualified_object_name,
        shared_helpers::TranslationDirection,
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

    fn translate_object_name(
        name: &sqlparser::ast::ObjectName,
        _schema: &ParserDB,
        _options: &Pg2SqliteOptions,
    ) -> Result<sqlparser::ast::ObjectName, Error> {
        Ok(sqlite_unqualified_object_name(name))
    }
}

define_direction_wrappers! {
    direction = Forward;
    fn translate_table_with_joins(table_with_joins: &TableWithJoins) -> TableWithJoins = translate_table_with_joins;
    fn translate_select_item(item: &SelectItem) -> SelectItem = translate_select_item;
    fn translate_order_by_expr(expr: &OrderByExpr) -> OrderByExpr = translate_order_by_expr;
    fn translate_connect_by_kinds(connect_by_kinds: &[ConnectByKind]) -> Vec<ConnectByKind> = translate_connect_by_kinds;
    fn translate_query_settings(settings: Option<&Vec<Setting>>) -> Option<Vec<Setting>> = translate_query_settings;
    fn translate_pipe_operators(pipe_operators: &[PipeOperator]) -> Vec<PipeOperator> = translate_pipe_operators;
    fn translate_with_clause(with: Option<&With>) -> Option<With> = translate_with_clause;
    fn translate_order_by_clause(order_by: Option<&OrderBy>) -> Option<OrderBy> = translate_order_by_clause;
    fn translate_limit_clause(limit_clause: Option<&LimitClause>) -> Option<LimitClause> = translate_limit_clause;
    fn translate_fetch_clause(fetch: Option<&Fetch>) -> Option<Fetch> = translate_fetch_clause;
    fn translate_group_by_expr(group_by: &GroupByExpr) -> GroupByExpr = translate_group_by_expr;
    fn translate_named_windows(named_windows: &[NamedWindowDefinition]) -> Vec<NamedWindowDefinition> = translate_named_windows;
    fn translate_values_rows(values: &Values) -> Values = translate_values_rows;
    fn translate_window_type(over: Option<&WindowType>) -> Option<WindowType> = translate_window_type;
}
