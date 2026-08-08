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
use sqlparser::ast::{Delete, Expr, Statement};

use super::helpers::{Forward, translate_table_with_joins};
use crate::{
    impls::{
        function_helpers::integer_literal,
        query_builder::single_expr_query,
        returning_scope::{delete_target, scope_returning_to_target},
        shared_helpers::translate_delete_core,
        translator_impls::postgis,
    },
    options::Pg2SqliteOptions,
    traits::translator::Translator,
};

impl Translator for Delete {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Statement;

    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        let (selection, from, returning, order_by, limit) =
            translate_delete_core::<Forward>(self, schema, options)?;

        let mut delete = Delete { selection, from, returning, order_by, limit, ..self.clone() };

        // Translated up front because the RETURNING scope check needs the
        // relations these introduce, whether or not the fold below runs.
        let translated_using = delete
            .using
            .take()
            .filter(|using| !using.is_empty())
            .map(|using| {
                using
                    .iter()
                    .map(|twj| translate_table_with_joins(twj, schema, options))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        delete.returning = scope_returning_to_target(
            delete.returning.take(),
            delete_target(&delete.from),
            translated_using.as_deref().unwrap_or_default(),
            schema,
            "USING",
        )?;

        if let Some(translated_using) = translated_using {
            // Convert DELETE FROM T USING U WHERE cond
            // to DELETE FROM T WHERE EXISTS (SELECT 1 FROM U WHERE cond)

            let original_selection = delete.selection;

            // `SELECT 1 FROM <using> WHERE <original predicate>`, the EXISTS
            // body that replaces the USING clause SQLite has no syntax for.
            //
            // An RLS table keeps its declared name here deliberately: the RLS
            // machinery emits a policy-filtering view under that name, and
            // PostgreSQL applies SELECT policies to a USING read, so the view
            // is the correct relation. Renaming to the backing table would
            // bypass the policies and strand the predicate's qualifiers.
            let subquery =
                single_expr_query(integer_literal(1), translated_using, original_selection);

            delete.selection = Some(Expr::Exists { subquery: Box::new(subquery), negated: false });
        }

        // Spatial predicate rewriting: route `ST_*` WHERE predicates over
        // indexed columns through the rtree shadow via an IN-subquery. The
        // helper rejects multi-source shapes naturally, so DELETE ... USING
        // (which by this point has its WHERE wrapped in EXISTS) falls through.
        if let Some(rewritten) = postgis::try_rewrite_spatial_delete(&delete, options)? {
            return Ok(Statement::Delete(rewritten));
        }

        Ok(Statement::Delete(delete))
    }
}
