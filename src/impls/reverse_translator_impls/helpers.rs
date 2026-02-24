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
    impls::shared_helpers::{self, TranslationDirection},
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

pub(super) fn reverse_translate_table_with_joins(
    table_with_joins: &TableWithJoins,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableWithJoins, Error> {
    shared_helpers::translate_table_with_joins::<Reverse>(table_with_joins, schema, options)
}

pub(super) fn reverse_translate_select_item(
    item: &SelectItem,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<SelectItem, Error> {
    shared_helpers::translate_select_item::<Reverse>(item, schema, options)
}

pub(super) fn reverse_translate_order_by_expr(
    expr: &OrderByExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<OrderByExpr, Error> {
    shared_helpers::translate_order_by_expr::<Reverse>(expr, schema, options)
}

pub(super) fn reverse_translate_connect_by_kinds(
    connect_by_kinds: &[ConnectByKind],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<ConnectByKind>, Error> {
    shared_helpers::translate_connect_by_kinds::<Reverse>(connect_by_kinds, schema, options)
}

pub(super) fn reverse_translate_query_settings(
    settings: Option<&Vec<Setting>>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Vec<Setting>>, Error> {
    shared_helpers::translate_query_settings::<Reverse>(settings, schema, options)
}

pub(super) fn reverse_translate_pipe_operators(
    pipe_operators: &[PipeOperator],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<PipeOperator>, Error> {
    shared_helpers::translate_pipe_operators::<Reverse>(pipe_operators, schema, options)
}

pub(super) fn reverse_translate_with_clause(
    with: Option<&With>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<With>, Error> {
    shared_helpers::translate_with_clause::<Reverse>(with, schema, options)
}

pub(super) fn reverse_translate_order_by_clause(
    order_by: Option<&OrderBy>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<OrderBy>, Error> {
    shared_helpers::translate_order_by_clause::<Reverse>(order_by, schema, options)
}

pub(super) fn reverse_translate_limit_clause(
    limit_clause: Option<&LimitClause>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<LimitClause>, Error> {
    shared_helpers::translate_limit_clause::<Reverse>(limit_clause, schema, options)
}

pub(super) fn reverse_translate_fetch_clause(
    fetch: Option<&Fetch>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Fetch>, Error> {
    shared_helpers::translate_fetch_clause::<Reverse>(fetch, schema, options)
}

pub(super) fn reverse_translate_group_by_expr(
    group_by: &GroupByExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<GroupByExpr, Error> {
    shared_helpers::translate_group_by_expr::<Reverse>(group_by, schema, options)
}

pub(super) fn reverse_translate_named_windows(
    named_windows: &[NamedWindowDefinition],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<NamedWindowDefinition>, Error> {
    shared_helpers::translate_named_windows::<Reverse>(named_windows, schema, options)
}

pub(super) fn reverse_translate_values_rows(
    values: &Values,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Values, Error> {
    shared_helpers::translate_values_rows::<Reverse>(values, schema, options)
}

pub(super) fn reverse_translate_window_type(
    over: Option<&WindowType>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<WindowType>, Error> {
    match over {
        None => Ok(None),
        Some(WindowType::NamedWindow(name)) => Ok(Some(WindowType::NamedWindow(name.clone()))),
        Some(WindowType::WindowSpec(spec)) => {
            Ok(Some(WindowType::WindowSpec(shared_helpers::translate_window_spec::<Reverse>(
                spec, schema, options,
            )?)))
        }
    }
}
