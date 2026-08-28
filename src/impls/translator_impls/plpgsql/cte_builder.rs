//! CTE (Common Table Expression) builder for PL/pgSQL translation.
//!
//! This module provides utilities for constructing CTEs that represent
//! variable bindings and combining them into WITH clauses.

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

use sqlparser::ast::{
    Cte, Expr, Ident, SelectItem, SetExpr, TableAlias, TableAliasColumnDef, With,
    helpers::attached_token::AttachedToken,
};

use super::VariableBinding;
use crate::impls::query_builder::{make_query, make_simple_select};

/// Builder for constructing CTEs from variable bindings.
pub struct CteBuilder;

impl CteBuilder {
    /// Creates a CTE for a variable binding.
    ///
    /// Transforms `variable := expression` into:
    /// ```sql
    /// variable(val) AS (SELECT expression FROM <dependencies>)
    /// ```
    ///
    /// `from` names the variable CTEs the expression itself reads, which is
    /// what lets one variable be defined in terms of another. Left empty the
    /// body reads no relation, which is right for a literal or a `NEW`
    /// reference.
    #[must_use]
    pub fn create_variable_cte(
        binding: &VariableBinding,
        expr: Expr,
        from: Vec<sqlparser::ast::TableWithJoins>,
    ) -> Cte {
        let select = make_simple_select(vec![SelectItem::UnnamedExpr(expr)], from, None);
        let query = make_query(None, SetExpr::Select(Box::new(select)));
        Cte {
            alias: TableAlias {
                name: Ident::new(binding.name.clone()),
                columns: vec![TableAliasColumnDef::from_name("val")],
                explicit: false,
                at: None,
            },
            query: Box::new(query),
            from: None,
            materialized: None,
            closing_paren_token: AttachedToken::empty(),
        }
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

#[cfg(all(test, feature = "std"))]
mod tests {
    use sqlparser::ast::{Expr, Value, ValueWithSpan};

    use super::{CteBuilder, VariableBinding};

    #[test]
    fn create_ctes_and_references_work() {
        let binding =
            VariableBinding { name: "v_id".to_string(), expression: "uuidv7()".to_string() };
        let expr = Expr::Value(ValueWithSpan::from(Value::Number("1".to_string(), false)));

        let cte = CteBuilder::create_variable_cte(&binding, expr.clone(), Vec::new());
        assert_eq!(cte.alias.name.value, "v_id");
        assert_eq!(cte.alias.columns.len(), 1);

        let second = VariableBinding { name: "v_simple".to_string(), expression: "1".to_string() };
        let simple = CteBuilder::create_variable_cte(&second, expr, Vec::new());
        assert_eq!(simple.alias.name.value, "v_simple");

        let with_none = CteBuilder::combine_ctes(Vec::new());
        assert!(with_none.is_none());

        let with_some = CteBuilder::combine_ctes(vec![cte, simple]).unwrap();
        assert_eq!(with_some.cte_tables.len(), 2);

        assert_eq!(CteBuilder::cte_column_reference("v_id", "val").to_string(), "v_id.val");
        assert_eq!(CteBuilder::variable_reference("v_simple").to_string(), "v_simple.val");
    }
}
