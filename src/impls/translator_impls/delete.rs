use sql_traits::structs::ParserDB;
use sqlparser::{
    ast::{
        Delete, Expr, GroupByExpr, Ident, ObjectName, Query, Select, SelectFlavor, SelectItem,
        SetExpr, Statement, TableFactor, Value, ValueWithSpan,
        helpers::attached_token::AttachedToken,
    },
    tokenizer::Span,
};

use crate::{
    impls::translator_impls::rls::table_has_rls,
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
        let mut delete = self.clone();

        if let Some(using) = delete.using.take().filter(|u| !u.is_empty()) {
            // Convert DELETE FROM T USING U WHERE cond
            // to DELETE FROM T WHERE EXISTS (SELECT 1 FROM U WHERE cond)

            // Keep the original selection (WHERE clause)
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
                    from: using, // The tables from USING go here
                    lateral_views: vec![],
                    selection: original_selection, // The WHERE clause moves here
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
                        let table_name = name.to_string();
                        if table_has_rls(&table_name, schema) {
                            let new_name = format!("{table_name}{rls_suffix}");
                            *name = ObjectName::from(vec![Ident::new(new_name)]);
                        }
                    }

                    // Update JOINed table references
                    for join in &mut table_with_joins.joins {
                        if let TableFactor::Table { name, .. } = &mut join.relation {
                            let table_name = name.to_string();
                            if table_has_rls(&table_name, schema) {
                                let new_name = format!("{table_name}{rls_suffix}");
                                *name = ObjectName::from(vec![Ident::new(new_name)]);
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
