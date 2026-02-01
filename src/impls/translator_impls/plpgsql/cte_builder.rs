//! CTE (Common Table Expression) builder for PL/pgSQL translation.
//!
//! This module provides utilities for constructing CTEs that represent
//! variable bindings and combining them into WITH clauses.

use sqlparser::ast::{
    Cte, Expr, GroupByExpr, Ident, Query, Select, SelectFlavor, SelectItem, SetExpr, TableAlias,
    TableAliasColumnDef, With, helpers::attached_token::AttachedToken,
};

use super::context::VariableBinding;

/// Builder for constructing CTEs from variable bindings.
pub struct CteBuilder;

impl CteBuilder {
    /// Creates a CTE for a variable binding.
    ///
    /// Transforms `variable := expression` into:
    /// ```sql
    /// variable(val) AS (SELECT expression)
    /// ```
    #[must_use]
    pub fn create_variable_cte(binding: &VariableBinding, expr: Expr) -> Cte {
        Cte {
            alias: TableAlias {
                name: Ident::new(binding.name.clone()),
                columns: vec![TableAliasColumnDef::from_name("val")],
                explicit: false,
            },
            query: Box::new(Query {
                with: None,
                body: Box::new(SetExpr::Select(Box::new(Select {
                    select_token: AttachedToken::empty(),
                    distinct: None,
                    top: None,
                    top_before_distinct: false,
                    projection: vec![SelectItem::UnnamedExpr(expr)],
                    into: None,
                    from: vec![],
                    lateral_views: vec![],
                    selection: None,
                    group_by: GroupByExpr::Expressions(vec![], vec![]),
                    cluster_by: vec![],
                    distribute_by: vec![],
                    sort_by: vec![],
                    having: None,
                    named_window: vec![],
                    qualify: None,
                    window_before_qualify: false,
                    value_table_mode: None,
                    connect_by: None,
                    flavor: SelectFlavor::Standard,
                    exclude: None,
                    optimizer_hint: None,
                    prewhere: None,
                }))),
                order_by: None,
                limit_clause: None,
                fetch: None,
                locks: vec![],
                for_clause: None,
                settings: None,
                format_clause: None,
                pipe_operators: vec![],
            }),
            from: None,
            materialized: None,
            closing_paren_token: AttachedToken::empty(),
        }
    }

    /// Creates a simple CTE from a name and expression.
    #[must_use]
    pub fn create_simple_cte(name: &str, expr: Expr) -> Cte {
        let binding = VariableBinding {
            name: name.to_string(),
            expression: String::new(), // Not used in this path
        };
        Self::create_variable_cte(&binding, expr)
    }

    /// Combines multiple CTEs into a WITH clause.
    #[must_use]
    pub fn combine_ctes(ctes: Vec<Cte>) -> Option<With> {
        if ctes.is_empty() {
            None
        } else {
            Some(With { with_token: AttachedToken::empty(), recursive: false, cte_tables: ctes })
        }
    }

    /// Creates a reference expression to a CTE column.
    ///
    /// For a CTE named "`v_id`" with column "val", returns `v_id.val`
    #[must_use]
    pub fn cte_column_reference(cte_name: &str, column_name: &str) -> Expr {
        Expr::CompoundIdentifier(vec![
            Ident::new(cte_name.to_string()),
            Ident::new(column_name.to_string()),
        ])
    }

    /// Creates a reference to a variable CTE's value.
    ///
    /// For a variable CTE named "`v_id`", returns `v_id.val`
    #[must_use]
    pub fn variable_reference(var_name: &str) -> Expr {
        Self::cte_column_reference(var_name, "val")
    }
}
