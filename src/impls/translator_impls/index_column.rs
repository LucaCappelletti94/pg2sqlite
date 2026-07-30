//! Implementation of the [`Translator`] trait for the
//! `IndexColumn` type.

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
use sqlparser::ast::IndexColumn;

use crate::prelude::{Pg2SqliteOptions, Translator};

impl Translator for IndexColumn {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Self;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // SQLite has no operator classes, so the clause cannot be emitted:
        // `CREATE INDEX i ON t (s text_pattern_ops)` is `near
        // "text_pattern_ops": syntax error`. Dropping it does not change which
        // rows the database accepts, even on a UNIQUE index, where an opclass
        // is the one thing that could: PostgreSQL's pattern classes compare
        // bitwise while the default compares by collation, and those disagree
        // only under a nondeterministic collation, which `Expr::Collate`
        // already refuses.
        if self.operator_class.is_some() {
            crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
                construct: "index operator class",
                reason: "SQLite has no operator classes, so the index serves fewer queries than \
                         the PostgreSQL one, notably the pattern matches a text pattern class \
                         exists for.",
            });
        }

        // Every field is listed, with no `..self.clone()`, so a field added
        // upstream fails to compile here instead of reaching SQLite unexamined.
        Ok(IndexColumn { column: self.column.translate(schema, options)?, operator_class: None })
    }
}
