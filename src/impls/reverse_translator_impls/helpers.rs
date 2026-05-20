//! Shared helper functions for reverse translation of table references,
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
use sqlparser::ast::{Expr, Query, TableWithJoins, WindowType};

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

    fn translate_insert(
        insert: &sqlparser::ast::Insert,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<sqlparser::ast::Insert, Error> {
        insert.reverse_translate(schema, options)
    }

    fn translate_delete(
        delete: &sqlparser::ast::Delete,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Result<sqlparser::ast::Delete, Error> {
        delete.reverse_translate(schema, options)
    }
}

define_direction_wrappers! {
    direction = Reverse;
    fn reverse_translate_table_with_joins(table_with_joins: &TableWithJoins) -> TableWithJoins = translate_table_with_joins;
    fn reverse_translate_window_type(over: Option<&WindowType>) -> Option<WindowType> = translate_window_type;
}
