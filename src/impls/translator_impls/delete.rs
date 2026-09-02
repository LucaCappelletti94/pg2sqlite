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

use sqlparser::ast::{Delete, Expr, Statement};

use super::helpers::{Forward, translate_table_with_joins};
use crate::impls::{
    function_helpers::integer_literal,
    query_builder::single_expr_query,
    returning_scope::{delete_target, scope_returning_to_target},
    shared_helpers::translate_delete_core,
    translator_impls::postgis,
};

crate::traits::translator::impl_contextual_translator!(Delete => Statement);
/// Every relation a `DELETE` lists, so a reference qualified by a `USING`
/// relation resolves as well as one naming the target.
pub(crate) fn delete_scope_query(delete: &Delete) -> sqlparser::ast::Query {
    let (sqlparser::ast::FromTable::WithFromKeyword(tables)
    | sqlparser::ast::FromTable::WithoutKeyword(tables)) = &delete.from;
    let mut relations = tables.clone();
    if let Some(using) = &delete.using {
        relations.extend(using.iter().cloned());
    }
    crate::impls::shared_helpers::relations_scope_query(relations)
}

impl crate::traits::translator::TranslatorWithContext for Delete {
    fn translate_with_warnings(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // The statement's target is the relation an unqualified column names,
        // and a query inside the statement attaches its own scope over this
        // one, so an outer reference still resolves.
        let scope_query = delete_scope_query(self);
        let target_scope =
            Some(sql_traits::structs::ColumnScope::from_query(&scope_query, schema)?);
        let scoped = target_scope.as_ref().map(|scope| options.with_scope(scope));
        let options = scoped.as_ref().unwrap_or(options);
        let (selection, from, returning, order_by, limit) =
            translate_delete_core::<Forward>(self, schema, options, emit)?;

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
                    .map(|twj| translate_table_with_joins(twj, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        delete.returning = scope_returning_to_target(
            delete.returning.take(),
            delete_target(&delete.from),
            translated_using.as_deref().unwrap_or_default(),
            schema,
            options.schema_is_complete(),
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
        if let Some(rewritten) = postgis::try_rewrite_spatial_delete(&delete, options) {
            return Ok(Statement::Delete(rewritten));
        }

        Ok(Statement::Delete(delete))
    }
}
