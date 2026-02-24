//! Shared helper functions for forward translation of table references,
//! joins, and select items.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    ConnectByKind, Expr, OrderByExpr, PipeOperator, Query, SelectItem, Setting, TableWithJoins,
};

use crate::{
    errors::Error,
    impls::shared_helpers::{self, TranslationDirection},
    prelude::{Pg2SqliteOptions, Translator},
};

/// Forward (PostgreSQL → SQLite) translation direction.
pub(crate) struct Forward;

impl TranslationDirection for Forward {
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
