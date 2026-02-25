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
        object_name::sqlite_unqualified_object_name,
        shared_helpers::{self, TranslationDirection},
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

pub(super) fn translate_table_with_joins(
    table_with_joins: &TableWithJoins,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableWithJoins, Error> {
    shared_helpers::translate_table_with_joins::<Forward>(table_with_joins, schema, options)
}

pub(super) fn translate_select_item(
    item: &SelectItem,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<SelectItem, Error> {
    shared_helpers::translate_select_item::<Forward>(item, schema, options)
}

pub(super) fn translate_order_by_expr(
    expr: &OrderByExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<OrderByExpr, Error> {
    shared_helpers::translate_order_by_expr::<Forward>(expr, schema, options)
}

pub(super) fn translate_connect_by_kinds(
    connect_by_kinds: &[ConnectByKind],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<ConnectByKind>, Error> {
    shared_helpers::translate_connect_by_kinds::<Forward>(connect_by_kinds, schema, options)
}

pub(super) fn translate_query_settings(
    settings: Option<&Vec<Setting>>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Vec<Setting>>, Error> {
    shared_helpers::translate_query_settings::<Forward>(settings, schema, options)
}

pub(super) fn translate_pipe_operators(
    pipe_operators: &[PipeOperator],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<PipeOperator>, Error> {
    shared_helpers::translate_pipe_operators::<Forward>(pipe_operators, schema, options)
}

pub(super) fn translate_with_clause(
    with: Option<&With>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<With>, Error> {
    shared_helpers::translate_with_clause::<Forward>(with, schema, options)
}

pub(super) fn translate_order_by_clause(
    order_by: Option<&OrderBy>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<OrderBy>, Error> {
    shared_helpers::translate_order_by_clause::<Forward>(order_by, schema, options)
}

pub(super) fn translate_limit_clause(
    limit_clause: Option<&LimitClause>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<LimitClause>, Error> {
    shared_helpers::translate_limit_clause::<Forward>(limit_clause, schema, options)
}

pub(super) fn translate_fetch_clause(
    fetch: Option<&Fetch>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<Fetch>, Error> {
    shared_helpers::translate_fetch_clause::<Forward>(fetch, schema, options)
}

pub(super) fn translate_group_by_expr(
    group_by: &GroupByExpr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<GroupByExpr, Error> {
    shared_helpers::translate_group_by_expr::<Forward>(group_by, schema, options)
}

pub(super) fn translate_named_windows(
    named_windows: &[NamedWindowDefinition],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Vec<NamedWindowDefinition>, Error> {
    shared_helpers::translate_named_windows::<Forward>(named_windows, schema, options)
}

pub(super) fn translate_values_rows(
    values: &Values,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Values, Error> {
    shared_helpers::translate_values_rows::<Forward>(values, schema, options)
}

pub(super) fn translate_window_type(
    over: Option<&WindowType>,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<WindowType>, Error> {
    match over {
        None => Ok(None),
        Some(WindowType::NamedWindow(name)) => Ok(Some(WindowType::NamedWindow(name.clone()))),
        Some(WindowType::WindowSpec(spec)) => {
            Ok(Some(WindowType::WindowSpec(shared_helpers::translate_window_spec::<Forward>(
                spec, schema, options,
            )?)))
        }
    }
}
