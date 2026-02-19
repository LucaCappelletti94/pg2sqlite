//! Shared helper functions for reverse translation of table references,
//! joins, and select items.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{Expr, Query, SelectItem, TableWithJoins};

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
