use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike},
};
use sqlparser::{
    ast::{
        Delete, Expr, FromTable, GroupByExpr, Query, Select, SelectFlavor, SelectItem, SetExpr,
        Statement, TableFactor, Value, ValueWithSpan, helpers::attached_token::AttachedToken,
    },
    tokenizer::Span,
};

use super::helpers::{Forward, translate_table_with_joins};
use crate::{
    impls::{
        object_name::{append_suffix, schema_and_table_for_lookup},
        shared_helpers::translate_returning,
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
        // Translate the WHERE clause up front
        let selection =
            self.selection.as_ref().map(|e| e.translate(schema, options)).transpose()?;

        // Translate the FROM clause
        let from = translate_from_table(&self.from, schema, options)?;

        // Translate RETURNING expressions
        let returning = translate_returning::<Forward>(self.returning.as_ref(), schema, options)?;

        let mut delete = Delete { selection, from, returning, ..self.clone() };

        if let Some(using) = delete.using.take().filter(|u| !u.is_empty()) {
            // Convert DELETE FROM T USING U WHERE cond
            // to DELETE FROM T WHERE EXISTS (SELECT 1 FROM U WHERE cond)

            // Translate USING tables
            let translated_using = using
                .iter()
                .map(|twj| translate_table_with_joins(twj, schema, options))
                .collect::<Result<Vec<_>, _>>()?;

            // Keep the already-translated selection (WHERE clause)
            let original_selection = delete.selection;

            // Create the subquery
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
                    optimizer_hint: None,
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
                    // Update main table reference
                    if let TableFactor::Table { name, .. } = &mut table_with_joins.relation {
                        let (table_schema, table_name) = schema_and_table_for_lookup(name);
                        if table_name
                            .and_then(|table_name| schema.table(table_schema, table_name))
                            .is_some_and(|table| table.has_row_level_security(schema))
                        {
                            *name = append_suffix(name, rls_suffix);
                        }
                    }

                    // Update JOINed table references
                    for join in &mut table_with_joins.joins {
                        if let TableFactor::Table { name, .. } = &mut join.relation {
                            let (table_schema, table_name) = schema_and_table_for_lookup(name);
                            if table_name
                                .and_then(|table_name| schema.table(table_schema, table_name))
                                .is_some_and(|table| table.has_row_level_security(schema))
                            {
                                *name = append_suffix(name, rls_suffix);
                            }
                        }
                    }
                }
            }

            // New selection is EXISTS(subquery)
            delete.selection = Some(Expr::Exists { subquery: Box::new(subquery), negated: false });

            // Clear USING
            delete.using = None;
        }

        Ok(Statement::Delete(delete))
    }
}

fn translate_from_table(
    from: &FromTable,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<FromTable, crate::errors::Error> {
    Ok(match from {
        FromTable::WithFromKeyword(tables) => {
            FromTable::WithFromKeyword(
                tables
                    .iter()
                    .map(|t| translate_table_with_joins(t, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        FromTable::WithoutKeyword(tables) => {
            FromTable::WithoutKeyword(
                tables
                    .iter()
                    .map(|t| translate_table_with_joins(t, schema, options))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
    })
}
