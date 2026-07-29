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
use sqlparser::{
    ast::{
        Delete, Expr, GroupByExpr, Query, Select, SelectFlavor, SelectItem, SetExpr, Statement,
        TableFactor, Value, ValueWithSpan, helpers::attached_token::AttachedToken,
    },
    tokenizer::Span,
};

use super::helpers::{Forward, translate_table_with_joins};
use crate::{
    impls::{
        object_name::{append_suffix, table_has_implicit_public_rls},
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

            let mut subquery = Query {
                with: None,
                body: Box::new(SetExpr::Select(Box::new(Select {
                    select_token: AttachedToken::empty(),
                    distinct: None,
                    top: None,
                    top_before_distinct: false,
                    projection: vec![SelectItem::UnnamedExpr(Expr::Value(ValueWithSpan {
                        value: Value::Number("1".to_string(), false),
                        span: Span::empty(),
                    }))],
                    into: None,
                    from: translated_using, // The translated USING tables go here
                    lateral_views: vec![],
                    selection: original_selection, // The translated WHERE clause moves here
                    group_by: GroupByExpr::Expressions(vec![], vec![]),
                    cluster_by: vec![],
                    distribute_by: vec![],
                    sort_by: vec![],
                    having: None,
                    named_window: vec![],
                    qualify: None,
                    connect_by: vec![],
                    window_before_qualify: false,
                    exclude: None,
                    optimizer_hints: Vec::new(),
                    value_table_mode: None,
                    prewhere: None,
                    flavor: SelectFlavor::Standard,
                    select_modifiers: None,
                }))),
                order_by: None,
                limit_clause: None,
                fetch: None,
                locks: vec![],
                for_clause: None,
                settings: None,
                format_clause: None,
                pipe_operators: vec![],
            };

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
