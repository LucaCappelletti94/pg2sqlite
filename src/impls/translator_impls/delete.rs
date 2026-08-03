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
use sqlparser::ast::{Delete, Expr, SetExpr, Statement, TableFactor};

use super::helpers::{Forward, translate_table_with_joins};
use crate::{
    impls::{
        function_helpers::integer_literal,
        object_name::{append_suffix, table_has_implicit_public_rls},
        query_builder::single_expr_query,
        shared_helpers::translate_delete_core,
        translator_impls::postgis,
    },
    options::Pg2SqliteOptions,
    traits::{TranslationOptions, translator::Translator},
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

        if let Some(using) = delete.using.take().filter(|u| !u.is_empty()) {
            // Convert DELETE FROM T USING U WHERE cond
            // to DELETE FROM T WHERE EXISTS (SELECT 1 FROM U WHERE cond)

            let translated_using = using
                .iter()
                .map(|twj| translate_table_with_joins(twj, schema, options))
                .collect::<Result<Vec<_>, _>>()?;

            let original_selection = delete.selection;

            // `SELECT 1 FROM <using> WHERE <original predicate>`, the EXISTS body
            // that replaces the USING clause SQLite has no syntax for.
            let mut subquery =
                single_expr_query(integer_literal(1), translated_using, original_selection);

            // Walk the FROM clause and update table names for RLS tables
            // Tables with RLS are renamed to table_rls (backing table), and we need
            // to reference the backing table in queries, not the view.
            let rls_suffix = options.get_rls_table_suffix();

            if let SetExpr::Select(ref mut select) = *subquery.body {
                for table_with_joins in &mut select.from {
                    if let TableFactor::Table { name, .. } = &mut table_with_joins.relation
                        && table_has_implicit_public_rls(schema, name)?
                    {
                        *name = append_suffix(name, rls_suffix);
                    }

                    for join in &mut table_with_joins.joins {
                        if let TableFactor::Table { name, .. } = &mut join.relation
                            && table_has_implicit_public_rls(schema, name)?
                        {
                            *name = append_suffix(name, rls_suffix);
                        }
                    }
                }
            }

            delete.selection = Some(Expr::Exists { subquery: Box::new(subquery), negated: false });

            delete.using = None;
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
