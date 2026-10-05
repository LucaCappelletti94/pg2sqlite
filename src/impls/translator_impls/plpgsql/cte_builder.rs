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
    Cte, Expr, Ident, ObjectName, ObjectNamePart, SelectItem, SetExpr, TableAlias,
    TableAliasColumnDef, With, helpers::attached_token::AttachedToken,
};

use crate::impls::query_builder::{
    from_relation, make_query, make_simple_select, plain_table_factor,
};

/// Builder for constructing CTEs from variable bindings.
pub struct CteBuilder;

/// The single column a variable CTE carries, holding the value bound to the
/// PL/pgSQL variable the CTE stands for.
pub(crate) const VARIABLE_VALUE_COLUMN: &str = "val";

impl CteBuilder {
    /// Creates a CTE for a variable binding.
    ///
    /// Transforms `variable := expression` into:
    /// ```sql
    /// variable(val) AS (SELECT expression)
    /// ```
    ///
    /// The body reads no relation: a variable this one is defined in terms of
    /// is read through [`Self::variable_reference`], which is a scalar
    /// subquery and so needs nothing in the `FROM` clause.
    #[must_use]
    pub fn create_variable_cte(name: &str, expr: Expr) -> Cte {
        let select = make_simple_select(vec![SelectItem::UnnamedExpr(expr)], Vec::new(), None);
        let query = make_query(None, SetExpr::Select(Box::new(select)));
        Cte {
            alias: TableAlias {
                name: Ident::new(name.to_string()),
                columns: vec![TableAliasColumnDef::from_name(VARIABLE_VALUE_COLUMN)],
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

    /// Reads a variable CTE's value, as `(SELECT val FROM v_id)`.
    ///
    /// A scalar subquery rather than a `v_id.val` column reference, because a
    /// column reference only resolves where the CTE is in the `FROM` clause of
    /// that same query level. The subquery resolves anywhere the CTE is in
    /// scope, which is every expression position of the statement carrying the
    /// `WITH`, nested derived tables and `ORDER BY` included, and it leaves the
    /// row shape of the query it appears in alone.
    #[must_use]
    pub fn variable_reference(var_name: &str) -> Expr {
        let select = make_simple_select(
            vec![SelectItem::UnnamedExpr(Expr::Identifier(Ident::new(VARIABLE_VALUE_COLUMN)))],
            from_relation(plain_table_factor(ObjectName(vec![ObjectNamePart::Identifier(
                Ident::new(var_name.to_string()),
            )]))),
            None,
        );
        Expr::Subquery(Box::new(make_query(None, SetExpr::Select(Box::new(select)))))
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sqlparser::ast::{Expr, Value, ValueWithSpan};

    use super::CteBuilder;

    #[test]
    fn create_ctes_and_references_work() {
        let expr = Expr::Value(ValueWithSpan::from(Value::Number("1".to_string(), false)));

        let cte = CteBuilder::create_variable_cte("v_id", expr.clone());
        assert_eq!(cte.alias.name.value, "v_id");
        assert_eq!(cte.alias.columns.len(), 1);
        assert_eq!(cte.query.to_string(), "SELECT 1");

        let simple = CteBuilder::create_variable_cte("v_simple", expr);
        assert_eq!(simple.alias.name.value, "v_simple");

        let with_none = CteBuilder::combine_ctes(Vec::new());
        assert!(with_none.is_none());

        let with_some = CteBuilder::combine_ctes(vec![cte, simple]).unwrap();
        assert_eq!(with_some.cte_tables.len(), 2);

        assert_eq!(
            CteBuilder::variable_reference("v_simple").to_string(),
            "(SELECT val FROM v_simple)"
        );
    }
}
