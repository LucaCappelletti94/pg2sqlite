//! Shared helper functions for reverse translation of table references,
//! joins, and select items.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    ConnectByKind, Expr, Fetch, GroupByExpr, LimitClause, NamedWindowDefinition, OrderBy,
    OrderByExpr, PipeOperator, Query, SelectItem, Setting, TableWithJoins, Values, WindowType,
    With,
};

use crate::{
    errors::Error,
    impls::{direction_wrappers::define_direction_wrappers, shared_helpers::TranslationDirection},
    prelude::{Pg2SqliteOptions, ReverseTranslator},
};

/// Reverse (SQLite → PostgreSQL) translation direction.
pub(crate) struct Reverse;

impl TranslationDirection for Reverse {
    fn translate_expr(
        expr: &Expr,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Expr, Error> {
        expr.reverse_translate(schema, options)
    }

    fn translate_query(
        query: &Query,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<Query, Error> {
        query.reverse_translate(schema, options)
    }
}

define_direction_wrappers! {
    direction = Reverse;
    fn reverse_translate_table_with_joins(table_with_joins: &TableWithJoins) -> TableWithJoins = translate_table_with_joins;
    fn reverse_translate_select_item(item: &SelectItem) -> SelectItem = translate_select_item;
    fn reverse_translate_order_by_expr(expr: &OrderByExpr) -> OrderByExpr = translate_order_by_expr;
    fn reverse_translate_connect_by_kinds(connect_by_kinds: &[ConnectByKind]) -> Vec<ConnectByKind> = translate_connect_by_kinds;
    fn reverse_translate_query_settings(settings: Option<&Vec<Setting>>) -> Option<Vec<Setting>> = translate_query_settings;
    fn reverse_translate_pipe_operators(pipe_operators: &[PipeOperator]) -> Vec<PipeOperator> = translate_pipe_operators;
    fn reverse_translate_with_clause(with: Option<&With>) -> Option<With> = translate_with_clause;
    fn reverse_translate_order_by_clause(order_by: Option<&OrderBy>) -> Option<OrderBy> = translate_order_by_clause;
    fn reverse_translate_limit_clause(limit_clause: Option<&LimitClause>) -> Option<LimitClause> = translate_limit_clause;
    fn reverse_translate_fetch_clause(fetch: Option<&Fetch>) -> Option<Fetch> = translate_fetch_clause;
    fn reverse_translate_group_by_expr(group_by: &GroupByExpr) -> GroupByExpr = translate_group_by_expr;
    fn reverse_translate_named_windows(named_windows: &[NamedWindowDefinition]) -> Vec<NamedWindowDefinition> = translate_named_windows;
    fn reverse_translate_values_rows(values: &Values) -> Values = translate_values_rows;
    fn reverse_translate_window_type(over: Option<&WindowType>) -> Option<WindowType> = translate_window_type;
}
