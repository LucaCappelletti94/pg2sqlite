//! Shared helper functions for forward translation of table references,
//! joins, and select items.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Expr, Query, SelectItem, TableWithJoins};

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
