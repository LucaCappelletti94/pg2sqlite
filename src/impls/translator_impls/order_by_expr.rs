//! Implementation of the [`Translator`] trait for the
//! `OrderByExpr` type.

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
use sqlparser::ast::OrderByExpr;

use crate::prelude::{Pg2SqliteOptions, Translator};

impl Translator for OrderByExpr {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Self;

    /// Only the `CREATE INDEX` column path reaches this. A query's `ORDER BY`
    /// goes through `shared_helpers::translate_order_by_expr`, where the
    /// `NULLS` qualifier is legal in SQLite and decides which rows come
    /// back, which is R48's subject. The two must not be made to share a
    /// rule.
    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        if self.with_fill.is_some() {
            return Err(crate::errors::Error::UnknownPostgresFeature(
                "WITH FILL in ORDER BY".to_string(),
            ));
        }

        // SQLite rejects a NULLS qualifier inside an index at every version,
        // measured on 3.51.1 as `unsupported use of NULLS LAST`, so it cannot be
        // emitted. Dropping it is safe: an index's null ordering decides which
        // orderings the index can SERVE, never which rows a query returns, so
        // the planner adds a sort step instead. `ASC` and `DESC` are legal here
        // and are kept.
        let mut index_options = self.options.clone();
        if index_options.nulls_first.take().is_some() {
            crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
                construct: "NULLS FIRST/LAST".to_string(),
                reason: "SQLite has no null ordering inside an index, so the index serves fewer \
                         orderings than the PostgreSQL one and a matching ORDER BY is sorted \
                         instead."
                    .to_string(),
            });
        }

        Ok(OrderByExpr {
            expr: self.expr.translate(schema, options)?,
            options: index_options,
            with_fill: None,
        })
    }
}
